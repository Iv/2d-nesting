# Vendored sparrow

Copy of [sparrow](https://github.com/JeroenGar/sparrow) 0.3.0, master `6f7b9d3`, MIT (see `LICENSE`).
Only `src/`, `Cargo.toml` (profiles removed) and `LICENSE` are kept.

Local changes (see the git history of this folder):

- Tilt penalty (`src/sample/tilt.rs`): per item, a penalty for each allowed rotation, given in
  `SampleConfig::tilt`. Collision-free placements with less penalty are preferred, a share of the
  samples is taken at zero-penalty rotations, and the initial left-bottom-fill counts the penalty as
  extra length. Without a table (`tilt: None`) sparrow behaves as upstream.
