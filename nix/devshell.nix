{ inputs, ... }:
{
  imports = [ inputs.devshell.flakeModule ];
  perSystem =
    { config, pkgs, ... }:
    let
      rust-toolchain = pkgs.rust-bin.fromRustupToolchainFile ../rust-toolchain.toml;
    in
    {
      devshells.default = {
        packages = [
          rust-toolchain
          config.treefmt.build.wrapper
          # The same mdformat treefmt drives, so `just readme` normalises its output
          # exactly the way the pre-commit hook would.
          config.treefmt.build.programs.mdformat
          config.hk-nix.package
          pkgs.cargo-watch
          pkgs.cargo-insta
          pkgs.deadnix
          pkgs.gifsicle
          pkgs.stdenv.cc
          pkgs.git
          pkgs.just
          pkgs.vhs
          config.packages.cargo-readme
        ];

        env = [
          {
            name = "RUST_BACKTRACE";
            value = "1";
          }
        ];

        devshell.motd = "";
        devshell.startup.hk.text = config.hk-nix.shellHook;
      };
    };
}
