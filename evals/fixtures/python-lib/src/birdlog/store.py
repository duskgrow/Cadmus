"""The observation store: one JSON line per observation in a single file."""

import json
import os
from dataclasses import asdict
from pathlib import Path

from birdlog.models import Observation

# Maximum number of observations the store holds; log() refuses beyond this.
MAX_OBSERVATIONS = 5000

# Filename of the default store, inside the birdlog directory.
DEFAULT_FILENAME = "sightings.jsonl"

# Directory the default store lives in, under the user's home.
DEFAULT_DIR = ".birdlog"


class Store:
    """A JSONL-backed store of observations."""

    def __init__(self, path: Path) -> None:
        self.path = path

    @classmethod
    def open_default(cls) -> "Store":
        """Opens the store under $BIRDLOG_HOME or ~/DEFAULT_DIR."""
        base = Path(os.environ.get("BIRDLOG_HOME", str(Path.home() / DEFAULT_DIR)))
        return cls(base / DEFAULT_FILENAME)

    def log(self, observation: Observation) -> None:
        """Appends one observation; refuses once MAX_OBSERVATIONS is reached."""
        observations = self.read_all()
        if len(observations) >= MAX_OBSERVATIONS:
            raise ValueError(f"store is full ({MAX_OBSERVATIONS} observations)")
        if observations and observations[-1] == observation:
            return  # 1.4.2: never store the same sighting twice in a row
        observations.append(observation)
        self.write_all(observations)

    def read_all(self) -> list[Observation]:
        """Reads every observation, oldest first."""
        if not self.path.exists():
            return []
        return [
            Observation(**json.loads(line))
            for line in self.path.read_text().splitlines()
        ]

    def write_all(self, observations: list[Observation]) -> None:
        """Rewrites the store file, one JSON line per observation."""
        self.path.write_text(
            "".join(json.dumps(asdict(o)) + "\n" for o in observations)
        )
