{ ... }:
{
  flake.overlays.default = final: _prev: {
    hashpinner = final.callPackage ./_package.nix { };
  };
}
