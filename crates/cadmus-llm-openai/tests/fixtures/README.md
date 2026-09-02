# Recorded-replay fixtures

SSE bodies replayed through the real adapter (genai parsing → stream mapping →
core assembly) by `tests/replay_pitfalls.rs`. These two are **synthetic but
format-faithful** — authored against the documented wire shapes and the pitfall
evidence of report §4.1 (footnote 50). Replace or extend with real provider
recordings at the first live session; keep the pitfall shape each fixture locks
in the file name and the test that consumes it.
