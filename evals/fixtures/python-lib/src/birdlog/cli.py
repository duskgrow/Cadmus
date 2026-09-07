"""The birdlog command-line interface: three subcommands."""

import argparse

from birdlog.models import Observation
from birdlog.search import find_observations
from birdlog.store import Store


def print_observation(observation: Observation) -> None:
    print(f"{observation.date}  {observation.species}  {observation.location}")


def main() -> None:
    parser = argparse.ArgumentParser(prog="birdlog")
    subcommands = parser.add_subparsers(dest="command", required=True)

    log_parser = subcommands.add_parser("log", help="record one observation")
    log_parser.add_argument("--species", required=True)
    log_parser.add_argument("--location", required=True)
    log_parser.add_argument("--date", required=True)
    log_parser.add_argument("--count", type=int, default=1)

    subcommands.add_parser("list", help="print every observation, oldest first")

    find_parser = subcommands.add_parser("find", help="search observations")
    find_parser.add_argument("query")

    args = parser.parse_args()
    store = Store.open_default()

    if args.command == "log":
        store.log(
            Observation(
                species=args.species,
                location=args.location,
                date=args.date,
                count=args.count,
            )
        )
    elif args.command == "list":
        for observation in store.read_all():
            print_observation(observation)
    elif args.command == "find":
        for observation in find_observations(store.read_all(), args.query):
            print_observation(observation)
