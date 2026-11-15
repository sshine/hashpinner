# Pin cargo-readme 3.4.1 independently of the nixpkgs channel. The git source
# carries its Cargo.lock, so the build is reproducible without a cargoHash.
{ inputs, ... }:
{
  perSystem =
    { pkgs, ... }:
    {
      packages.cargo-readme = pkgs.rustPlatform.buildRustPackage {
        pname = "cargo-readme";
        version = "3.4.1";
        src = inputs.cargo-readme-src;
        cargoLock.lockFile = "${inputs.cargo-readme-src}/Cargo.lock";
        # Upstream tests expect a full cargo-project fixture layout; skip.
        doCheck = false;
      };
    };
}
