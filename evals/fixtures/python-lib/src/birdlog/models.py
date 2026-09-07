"""The Observation dataclass: one record per sighting."""

from dataclasses import dataclass


@dataclass
class Observation:
    """A single birding observation."""

    species: str
    location: str
    date: str
    count: int = 1
    notes: str = ""
