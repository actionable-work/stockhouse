{
  lib,
  sourceRoot ? ./../../../..,
  ...
}: let
  # The thin-binary test inspects every binary crate, which per-crate source
  # isolation otherwise leaves out of this crate's test sandbox.
  binaries = lib.fileset.toSource {
    root = sourceRoot;
    fileset = sourceRoot + "/packages/rust/bins";
  };
in {
  test.env.STOCKHOUSE_BINS_ROOT = "${binaries}/packages/rust/bins";
}
