# Git hooks via hk-nix. The generated hk.pkl is symlinked into the repo root by
# the devshell startup hook (see devshell.nix); hk.pkl is gitignored.
{ inputs, ... }:
{
  imports = [ inputs.hk-nix.flakeModules.default ];
  perSystem =
    {
      config,
      pkgs,
      lib,
      ...
    }:
    let
      # Flags to reproduce the committed README.md from README.tpl and the CLI docs.
      # --input is required because the crate has a lib target: cargo-readme would
      # otherwise read lib.rs.
      readmeArgs = "--project-root crates/hashpinner --input src/main.rs --template ../../README.tpl";

      # Reference tools by absolute store path: the `nix flake check` hk-check sandbox
      # runs hooks without the devshell PATH, so a bare `treefmt` is not found there.
      treefmt = lib.getExe config.treefmt.build.wrapper;

      # Called by store path rather than via `cargo readme`, so the cargo-subcommand
      # argv has to be supplied by hand: without it clap only prints its usage.
      cargo-readme = "${lib.getExe' config.packages.cargo-readme "cargo-readme"} readme";
    in
    {
      hk-nix.settings.hooks = {
        "pre-commit" = {
          fix = true;
          stash = "git";
          steps.treefmt = {
            check = "${treefmt} --fail-on-change --no-cache {{files}}";
            fix = "${treefmt} {{files}}";
          };
        };

        "pre-push".steps = {
          deadnix = {
            glob = "*.nix";
            check = "${lib.getExe pkgs.deadnix} --fail {{files}}";
          };
          clippy = {
            check = "cargo clippy --all-targets --all-features -- -D warnings";
          };
          readme = {
            check = "${cargo-readme} ${readmeArgs} | diff - README.md";
            fix = "${cargo-readme} ${readmeArgs} -o README.md";
          };
          lock-check = {
            check = "cargo metadata --locked --format-version 1 > /dev/null";
          };
        };

        # hk runs this one itself, so no tool needs pinning. Note it has no
        # equivalent of --require-scope; --allowed-types is the only policy knob.
        "commit-msg".steps.conventional.builtin = config.hk-nix.builtins.check_conventional_commit;
      };
    };
}
