from __future__ import annotations

from collections import deque
from functools import lru_cache
from pathlib import Path
from typing import Iterable, Sequence
import json


DATA_ROOT = Path(__file__).resolve().parent
CITY_CODES_PATH = DATA_ROOT / "geant_city_codes.json"
LINKS_PATH = DATA_ROOT / "geant_links.json"


@lru_cache(maxsize=1)
def _load_city_codes() -> tuple[str, ...]:
    raw_data = json.loads(CITY_CODES_PATH.read_text(encoding="utf-8"))
    if not isinstance(raw_data, list) or not all(isinstance(value, str) for value in raw_data):
        raise ValueError(f"Invalid GEANT city-code data in {CITY_CODES_PATH}")
    return tuple(value.upper() for value in raw_data)


@lru_cache(maxsize=1)
def _load_links() -> tuple[tuple[str, str], ...]:
    raw_data = json.loads(LINKS_PATH.read_text(encoding="utf-8"))
    if not isinstance(raw_data, list):
        raise ValueError(f"Invalid GEANT link data in {LINKS_PATH}")

    links: list[tuple[str, str]] = []
    for entry in raw_data:
        if not isinstance(entry, list) or len(entry) != 2 or not all(isinstance(value, str) for value in entry):
            raise ValueError(f"Invalid GEANT link entry in {LINKS_PATH}: {entry!r}")
        links.append((entry[0].upper(), entry[1].upper()))
    return tuple(links)


GEANT_CITY_CODES = _load_city_codes()
GEANT_LINKS = _load_links()
DEFAULT_GEANT_ORIGIN_CITY = GEANT_CITY_CODES[0]
_CITY_ORDER = {city: index for index, city in enumerate(GEANT_CITY_CODES)}


def parse_geant_cities(raw: str | Sequence[str] | None, *, default_count: int | None = None) -> list[str]:
    if raw is None or raw == "":
        if default_count is None:
            return list(GEANT_CITY_CODES)
        return list(GEANT_CITY_CODES[:default_count])

    if isinstance(raw, str):
        candidates = [token.strip().upper() for token in raw.split(",") if token.strip()]
    else:
        candidates = [str(token).strip().upper() for token in raw if str(token).strip()]

    if not candidates:
        raise ValueError("GEANT city selection must not be empty")

    seen: set[str] = set()
    selected: list[str] = []
    for city in candidates:
        if city not in _CITY_ORDER:
            raise ValueError(f"Unknown GEANT city code: {city}")
        if city in seen:
            continue
        selected.append(city)
        seen.add(city)
    return selected


def serialize_geant_cities(cities: Sequence[str]) -> str:
    return ",".join(parse_geant_cities(cities))


def geant_default_client_cities(client_count: int) -> list[str]:
    if client_count < 0:
        raise ValueError("client_count must be non-negative")
    upper_bound = 1 + client_count
    if upper_bound > len(GEANT_CITY_CODES):
        raise ValueError(f"Requested {client_count} clients but GEANT only defines {len(GEANT_CITY_CODES) - 1} client cities")
    return list(GEANT_CITY_CODES[1:upper_bound])


def geant_selected_links(selected_cities: Sequence[str]) -> list[tuple[str, str]]:
    selected = set(parse_geant_cities(selected_cities))
    return [(city_a, city_b) for city_a, city_b in GEANT_LINKS if city_a in selected and city_b in selected]


def geant_adjacency(selected_cities: Sequence[str]) -> dict[str, list[str]]:
    ordered_cities = parse_geant_cities(selected_cities)
    graph = {city: [] for city in ordered_cities}
    for city_a, city_b in geant_selected_links(ordered_cities):
        graph[city_a].append(city_b)
        graph[city_b].append(city_a)
    for neighbors in graph.values():
        neighbors.sort(key=_CITY_ORDER.__getitem__)
    return graph


def geant_shortest_path(selected_cities: Sequence[str], start_city: str, end_city: str) -> list[str]:
    start = start_city.upper()
    end = end_city.upper()
    graph = geant_adjacency(selected_cities)
    if start not in graph or end not in graph:
        raise ValueError(f"GEANT path endpoint missing from selected city set: {start}->{end}")

    queue: deque[str] = deque([start])
    parent: dict[str, str | None] = {start: None}
    while queue:
        current = queue.popleft()
        if current == end:
            break
        for neighbor in graph[current]:
            if neighbor in parent:
                continue
            parent[neighbor] = current
            queue.append(neighbor)

    if end not in parent:
        raise ValueError(f"No GEANT path from {start} to {end}")

    path = [end]
    cursor = end
    while cursor != start:
        previous = parent[cursor]
        if previous is None:
            raise ValueError(f"GEANT path reconstruction failed for {start}->{end}")
        cursor = previous
        path.append(cursor)
    path.reverse()
    return path


def geant_shortest_path_closure(
    client_count: int,
    *,
    origin_city: str = DEFAULT_GEANT_ORIGIN_CITY,
    client_cities: Sequence[str] | None = None,
) -> list[str]:
    origin = origin_city.upper()
    requested_clients = parse_geant_cities(client_cities) if client_cities is not None else geant_default_client_cities(client_count)

    ordered = [origin]
    seen = {origin}
    for city in requested_clients:
        if city == origin or city in seen:
            continue
        ordered.append(city)
        seen.add(city)

    full_graph_cities = list(GEANT_CITY_CODES)
    for client_city in requested_clients:
        for city in geant_shortest_path(full_graph_cities, origin, client_city):
            if city in seen:
                continue
            ordered.append(city)
            seen.add(city)

    return ordered


def geant_parent_tree(selected_cities: Sequence[str], *, start_city: str = DEFAULT_GEANT_ORIGIN_CITY) -> dict[str, str | None]:
    start = start_city.upper()
    graph = geant_adjacency(selected_cities)
    if start not in graph:
        raise ValueError(f"Start city {start} is not part of the selected GEANT topology")

    parent: dict[str, str | None] = {start: None}
    queue: deque[str] = deque([start])
    while queue:
        current = queue.popleft()
        for neighbor in graph[current]:
            if neighbor in parent:
                continue
            parent[neighbor] = current
            queue.append(neighbor)
    return parent


def geant_path_from_parent_tree(parent_tree: dict[str, str | None], end_city: str) -> list[str]:
    end = end_city.upper()
    if end not in parent_tree:
        raise ValueError(f"No GEANT parent-tree entry for {end}")

    path = [end]
    cursor = end
    while True:
        previous = parent_tree[cursor]
        if previous is None:
            break
        path.append(previous)
        cursor = previous
    path.reverse()
    return path


def geant_city_to_node_index(selected_cities: Sequence[str]) -> dict[str, int]:
    return {city: index for index, city in enumerate(parse_geant_cities(selected_cities), start=1)}


def geant_router_name(selected_cities: Sequence[str], city: str) -> str:
    node_index = geant_city_to_node_index(selected_cities)[city.upper()]
    return f"r{node_index + 1}"


def geant_router_host(selected_cities: Sequence[str], city: str) -> str:
    node_index = geant_city_to_node_index(selected_cities)[city.upper()]
    return f"13.0.{node_index}.1"


def geant_backbone_interface_map(selected_cities: Sequence[str]) -> dict[str, dict[str, str]]:
    ordered_cities = parse_geant_cities(selected_cities)
    interface_counts = {city: 2 for city in ordered_cities}
    mapping = {city: {} for city in ordered_cities}

    for city_a, city_b in geant_selected_links(ordered_cities):
        mapping[city_a][city_b] = f"eth{interface_counts[city_a]}"
        mapping[city_b][city_a] = f"eth{interface_counts[city_b]}"
        interface_counts[city_a] += 1
        interface_counts[city_b] += 1

    return mapping


def geant_path_metric_names(
    selected_cities: Sequence[str],
    *,
    end_city: str,
    start_city: str = DEFAULT_GEANT_ORIGIN_CITY,
    direction: str = "tx",
) -> list[str]:
    path = geant_shortest_path(selected_cities, start_city, end_city)
    city_to_node_index = geant_city_to_node_index(selected_cities)
    interface_map = geant_backbone_interface_map(selected_cities)

    metric_names: list[str] = []
    for current_city, next_city in zip(path[:-1], path[1:]):
        router_name = f"r{city_to_node_index[current_city] + 1}"
        interface_name = interface_map[current_city][next_city]
        metric_names.append(f"{router_name}_{interface_name}_{direction}_bytes")
    return metric_names