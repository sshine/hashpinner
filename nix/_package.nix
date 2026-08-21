# The build definition, shared by this flake's packages and by the overlay.
#
# Taking `pkgs` as an argument (rather than closing over this flake's own) is what
# lets the overlay build against the consumer's nixpkgs, so downstream can override
# and cross-compile it.
{
  lib,
  rustPlatform,
  makeWrapper,
  git,
  ...
}:
let
  workspace = lib.importTOML ../Cargo.toml;
  crate = lib.importTOML ../crates/hashpinner/Cargo.toml;

  pname = crate.package.name;
in
rustPlatform.buildRustPackage {
  inherit pname;
  version = workspace.workspace.package.version;

  # Naming the inputs explicitly keeps target/ and .direnv/ out of the store, and
  # means an unrelated edit does not invalidate the build.
  src = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.unions [
      ../Cargo.toml
      ../Cargo.lock
      ../crates
      ../README.md
    ];
  };
  cargoLock.lockFile = ../Cargo.lock;

  cargoBuildFlags = [
    "--package"
    pname
  ];

  nativeBuildInputs = [ makeWrapper ];

  # The tag resolver shells out; a bare `git` on the user's PATH is not something
  # a Nix-installed binary may assume exists.
  postInstall = ''
    wrapProgram $out/bin/${pname} \
      --prefix PATH : ${lib.makeBinPath [ git ]}
  '';

  meta = {
    inherit (crate.package) description;
    mainProgram = pname;
    license = with lib.licenses; [
      mit
      asl20
    ];
  };
}
