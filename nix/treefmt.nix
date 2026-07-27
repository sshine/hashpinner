{ inputs, ... }:
{
  imports = [ inputs.treefmt-nix.flakeModule ];
  perSystem =
    { pkgs, ... }:
    let
      rust-toolchain = pkgs.rust-bin.fromRustupToolchainFile ../rust-toolchain.toml;
    in
    {
      treefmt = {
        projectRootFile = "flake.nix";

        # Fixtures are byte-exact inputs to the round-trip tests; reformatting them
        # would rewrite the very whitespace those tests exist to protect.
        settings.global.excludes = [ "crates/hashpinner-core/tests/fixtures/*" ];

        programs.nixfmt.enable = true;
        programs.rustfmt = {
          enable = true;
          package = rust-toolchain;
        };
        programs.mdformat.enable = true;
      };
    };
}
