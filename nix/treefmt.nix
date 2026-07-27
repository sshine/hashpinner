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

        programs.nixfmt.enable = true;
        programs.rustfmt = {
          enable = true;
          package = rust-toolchain;
        };
        programs.mdformat.enable = true;
      };
    };
}
