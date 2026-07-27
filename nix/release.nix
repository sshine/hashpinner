# Cross-compilation for the release artifacts.
#
# The action downloads a statically linked musl binary because Forgejo runners are
# frequently Alpine-based: a glibc-linked binary dies there with a loader error that
# says nothing useful. Static musl removes that whole class of failure.
#
# Building through cargo with rust-overlay's target support, rather than through
# pkgsCross.*.rustPlatform, keeps this to seconds: every dependency is pure Rust, so
# the only thing missing for a foreign target is a linker.
{ ... }:
{
  perSystem =
    { pkgs, lib, ... }:
    let
      # rustc finds musl's crt objects in its own `self-contained` directory and can
      # link x86_64 unaided; only a foreign architecture needs a cross linker.
      aarch64-cc = pkgs.pkgsCross.aarch64-multiplatform-musl.stdenv.cc;
      aarch64-linker = "${aarch64-cc}/bin/aarch64-unknown-linux-musl-cc";
    in
    {
      devshells.default = {
        packages = [ aarch64-cc ];

        env = [
          {
            name = "CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER";
            value = aarch64-linker;
          }
        ];
      };

      # Exposed so CI can resolve the linker without entering the devshell.
      packages.aarch64-musl-cc = aarch64-cc;

      apps.release-linker = {
        type = "app";
        program = lib.getExe (
          pkgs.writeShellApplication {
            name = "release-linker";
            text = ''echo "${aarch64-linker}"'';
          }
        );
      };
    };
}
