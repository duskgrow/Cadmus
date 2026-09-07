"""Tests for find_observations matching rules."""

from birdlog.models import Observation
from birdlog.search import find_observations


def test_find_matches_case_insensitively_on_species_and_location() -> None:
    observations = [
        Observation(
            species="Barn Swallow", location="old mill pond", date="2026-04-02"
        ),
        Observation(species="mallard", location="North Marsh", date="2026-04-03"),
    ]
    assert find_observations(observations, "swallow") == [observations[0]]
    assert find_observations(observations, "MARSH") == [observations[1]]
    assert find_observations(observations, "heron") == []
