"""Substring search over observations, case-insensitive on both sides."""

from birdlog.models import Observation

# TODO: support fuzzy matching


def find_observations(observations: list[Observation], query: str) -> list[Observation]:
    """Returns observations whose species or location contains `query`.

    Matching is case-insensitive on both sides. Returns an empty list when
    nothing matches.
    """
    needle = query.lower()
    return [
        observation
        for observation in observations
        if needle in observation.species.lower()
        or needle in observation.location.lower()
    ]
