use std::path::PathBuf;
use std::time::Instant;

use super::{
    config::{BenchConfig, DeltaKindCfg},
    csv::Csv,
    loader, metrics as fid,
    stats::{Series, SizeStats, TimeStats},
};

use crate::{
    chunk, demuxer::PointDemuxer, frame::PcfHeader, muxer::PointStreamMuxer, types::Flags,
};
use spatial_codecs::bench::runner::apply_resample;
use spatial_utils::point::Point3D;

use indicatif::{ProgressBar, ProgressStyle};

pub fn run(cfg: BenchConfig, out_csv: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
    let mut csv = if let Some(path) = out_csv {
        let mut c = Csv::create(path)?;
        c.header()?;
        Some(c)
    } else {
        None
    };

    for root in &cfg.datasets.roots {
        let frames = loader::discover_ply(PathBuf::from(root).as_path(), cfg.datasets.limit)?;
        if frames.is_empty() {
            eprintln!("No frames in {root}");
            continue;
        }

        println!(
            "== pcf benchmark ==\nDataset: {root}\nFrames  : {}\nStreams : {}\nGOP     : {}\nDelta   : {:?}\nMTU     : {}",
            frames.len(),
            cfg.pcf.streams,
            cfg.pcf.gop,
            cfg.pcf.delta,
            cfg.pcf.mtu
        );

        let pb = if cfg.progress {
            ProgressBar::new(frames.len() as u64).with_style(
                ProgressStyle::with_template("{bar:40.cyan/blue} {pos}/{len} {msg}")?
                    .progress_chars("##-"),
            )
        } else {
            ProgressBar::hidden()
        };

        for sweep in &cfg.sweeps {
            println!("\n-- Sweep: {} --", sweep.name);

            // per-sweep aggregators
            let mut tstats = TimeStats::default();
            let mut sstats = SizeStats::default();
            let mut rmse_series = Series::default();
            let mut psnr_series = Series::default();

            // per-stream state
            let mut muxers: Vec<PointStreamMuxer> =
                (0..cfg.pcf.streams).map(PointStreamMuxer::new).collect();
            for m in &mut muxers {
                m.gop.interval = cfg.pcf.gop;
                m.delta = match cfg.pcf.delta {
                    DeltaKindCfg::None => crate::diff::DeltaKind::None,
                    DeltaKindCfg::IndexAligned => crate::diff::DeltaKind::IndexAligned,
                };
            }
            let mut demux = PointDemuxer::new();

            let mut points_total: u64 = 0;
            let mut bytes_total: u64 = 0; // includes PCF + chunk headers (what actually "goes on the wire")
            let mut i_count = 0u64;
            let mut p_count = 0u64;

            // load all clouds upfront (deterministic timings)
            let mut clouds: Vec<Vec<Point3D>> = Vec::with_capacity(frames.len());
            for p in &frames {
                let mut pc = loader::load_points(p)?;
                if let Some(rs) = &cfg.datasets.resample {
                    pc = apply_resample(&pc, rs);
                }
                clouds.push(pc);
            }

            // warm-up: use mux path, not raw codec API (matches real flow)
            if cfg.warmup {
                let mut dummy_mux = PointStreamMuxer::new(u32::MAX);
                let mut tmp = Vec::new();
                for c in clouds.iter().take(3) {
                    dummy_mux.write_frame(c, Some(0), &sweep.params, &mut tmp)?;
                    tmp.clear();
                }
            }

            // stream local counters
            let mut stream_seq = vec![0u64; cfg.pcf.streams as usize];

            if cfg.progress {
                pb.set_position(0);
            }

            // main loop
            let t_begin = Instant::now();
            for (i, pc) in clouds.iter().enumerate() {
                pb.set_message(format!("frame {i}"));

                let sid = (i as u32) % cfg.pcf.streams;
                let seq = stream_seq[sid as usize];
                let mux = &mut muxers[sid as usize];

                let pts_ms = Some((i as u64) * 33); // synthetic PTS for testing
                let mut frame_buf = Vec::new();

                // Encode PCF frame (I or P)
                let t0 = Instant::now();
                mux.write_frame(pc, pts_ms, &sweep.params, &mut frame_buf)?;
                let t_enc = t0.elapsed();

                // Parse header to know exact header size and flags
                let hdr = PcfHeader::parse(&frame_buf).map_err(|e| format!("{e}"))?;
                let is_key = hdr.flags.contains(Flags::KEY);
                if is_key {
                    i_count += 1
                } else {
                    p_count += 1
                }

                // Chunk
                let t1 = Instant::now();
                let mut chunks = Vec::new();
                chunk::split_into_chunks(sid, seq, cfg.pcf.mtu, &frame_buf, &mut chunks);
                let t_chunk = t1.elapsed();

                // Exact header accounting
                let chunk_hdr_bytes = (chunks.len() as u64) * chunk::PKT_HEADER_LEN as u64;
                let pcf_hdr_bytes = (frame_buf.len() - hdr.payload.len()) as u64;

                sstats
                    .header_bytes
                    .push((chunk_hdr_bytes + pcf_hdr_bytes) as f64);
                sstats.frame_bytes.push(frame_buf.len() as f64);
                sstats.chunks.push(chunks.len() as f64);
                if is_key {
                    sstats.i_sizes.push(frame_buf.len() as f64);
                } else {
                    sstats.p_sizes.push(frame_buf.len() as f64);
                }

                bytes_total += frame_buf.len() as u64 + chunk_hdr_bytes;
                points_total += pc.len() as u64;

                // Reassemble (local emulation)
                let t2 = Instant::now();
                let mut reasm = chunk::Reassembler::new();
                let mut full = None;
                for c in &chunks {
                    if let Some((_rsid, _rseq, fr)) = reasm.push_chunk(c)? {
                        full = Some(fr);
                    }
                }
                let frame = full.expect("reassembler completed");
                let t_reasm = t2.elapsed();

                // Decode (demux)
                let t3 = Instant::now();
                let (_rsid, _rseq, _rpts, recon) = demux.push_frame(&frame)?;
                let t_dec = t3.elapsed();

                let t_e2e = t0.elapsed();

                // timings
                tstats.enc_ms.push_ms(t_enc);
                tstats.chunk_ms.push_ms(t_chunk);
                tstats.reasm_ms.push_ms(t_reasm);
                tstats.dec_ms.push_ms(t_dec);
                tstats.e2e_ms.push_ms(t_e2e);

                // fidelity (optional; index-aligned quick checks)
                if cfg.pcf.fidelity.rmse {
                    rmse_series.push(fid::rmse_index_aligned(pc, &recon));
                }
                if cfg.pcf.fidelity.psnr_y {
                    psnr_series.push(fid::psnr_y(pc, &recon));
                }

                // advance per-stream seq
                stream_seq[sid as usize] += 1;

                if cfg.progress {
                    pb.inc(1);
                }
            }

            let elapsed = t_begin.elapsed().as_secs_f64();
            let total_frame_bytes: f64 = sstats.frame_bytes.vals.iter().sum();
            let total_hdr_bytes: f64 = sstats.header_bytes.vals.iter().sum();
            let overhead_pct = if total_frame_bytes + total_hdr_bytes > 0.0 {
                100.0 * total_hdr_bytes / (total_frame_bytes + total_hdr_bytes)
            } else {
                0.0
            };

            // percentiles
            let enc_p50 = {
                let mut s = tstats.enc_ms.clone();
                s.pct(50.0)
            };
            let enc_p95 = {
                let mut s = tstats.enc_ms.clone();
                s.pct(95.0)
            };
            let dec_p50 = {
                let mut s = tstats.dec_ms.clone();
                s.pct(50.0)
            };
            let dec_p95 = {
                let mut s = tstats.dec_ms.clone();
                s.pct(95.0)
            };
            let e2e_p50 = {
                let mut s = tstats.e2e_ms.clone();
                s.pct(50.0)
            };
            let e2e_p95 = {
                let mut s = tstats.e2e_ms.clone();
                s.pct(95.0)
            };

            // sizes
            let avg_i = if sstats.i_sizes.is_empty() {
                0.0
            } else {
                sstats.i_sizes.mean()
            };
            let avg_p = if sstats.p_sizes.is_empty() {
                0.0
            } else {
                sstats.p_sizes.mean()
            };

            // throughputs
            let mbps = if elapsed > 0.0 {
                (bytes_total as f64 * 8.0) / elapsed / 1_000_000.0
            } else {
                0.0
            };
            let mpts = if elapsed > 0.0 {
                (points_total as f64) / elapsed / 1_000_000.0
            } else {
                0.0
            };

            // fidelity summary
            let rmse = if rmse_series.is_empty() {
                None
            } else {
                Some(rmse_series.mean())
            };
            let psnr_y = if psnr_series.is_empty() {
                None
            } else {
                Some(psnr_series.mean())
            };

            // print summary line
            println!(
                "frames={} I={} P={}  avgI={:.0}B avgP={:.0}B  enc_p50={:.3}ms dec_p50={:.3}ms e2e_p50={:.3}ms  mbps={:.2} mpts={:.2}  overhead={:.2}%{}{}",
                frames.len(),
                i_count,
                p_count,
                avg_i,
                avg_p,
                enc_p50,
                dec_p50,
                e2e_p50,
                mbps,
                mpts,
                overhead_pct,
                rmse.map(|v| format!(" rmse={v:.6}")).unwrap_or_default(),
                psnr_y.map(|v| if v.is_infinite() { " psnrY=inf".into() } else { format!(" psnrY={v:.3}dB") }).unwrap_or_default()
            );

            // CSV
            if let Some(c) = &mut csv {
                c.row(
                    root,
                    &sweep.name,
                    cfg.pcf.streams,
                    frames.len() as u64,
                    i_count,
                    p_count,
                    points_total,
                    bytes_total,
                    overhead_pct,
                    avg_i,
                    avg_p,
                    enc_p50,
                    enc_p95,
                    dec_p50,
                    dec_p95,
                    e2e_p50,
                    e2e_p95,
                    mbps,
                    mpts,
                    rmse,
                    psnr_y,
                )?;
            }

            if cfg.progress {
                pb.finish_with_message("done");
            }
        }
    }

    Ok(())
}
