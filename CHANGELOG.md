# Changelog

<!-- next-header -->

## git

## 0.1.1

- Bugfix: do not prune the final checkpoint
- Bugfix: eager checkpoints at GZIP block boundaries were broken; they should be
  at the first subsequent DEFLATE block start instead (after the GZIP block header).

## 0.1.0

- Initial release of the Rust version.
