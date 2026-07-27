# The webern/cargo-readme git version, which understands Cargo workspace
# inheritance; the crates.io release chokes on `version.workspace = true`.
{ inputs, ... }:
{
  perSystem =
    { pkgs, ... }:
    {
      packages.cargo-readme = pkgs.rustPlatform.buildRustPackage {
        pname = "cargo-readme";
        version = "git";
        src = inputs.cargo-readme-src;
        cargoLock.lockFile = "${inputs.cargo-readme-src}/Cargo.lock";
        # Upstream tests expect a full cargo project fixture layout; skip.
        doCheck = false;
      };
    };
}
