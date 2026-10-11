# Prebuilt release binaries, for consumers who want hashpinner without a Rust toolchain
# entering their build closure.
#
# The pinned version deliberately trails Cargo.toml: it names the last tag whose
# artifacts are published, not whatever the working tree happens to claim. Bump it
# once a tag's Release workflow has finished; `just release-hashes <tag>` prints the
# values.
#
# The hashes are of the tarballs the release publishes, so they can be checked against
# the .sha256 assets rather than taken on faith. Systems absent from `hashes` do not get
# the package, which is what keeps it off Darwin, since the release builds only Linux.
{ ... }:
{
  perSystem =
    {
      system,
      pkgs,
      lib,
      ...
    }:
    let
      release = {
        version = "0.2.0";
        hashes = {
          "x86_64-linux" = "sha256-UmJANHHbV5zVHB2Llwgiu2om0N7f9QrtZ73JH0aiUxc=";
          "aarch64-linux" = "sha256-1CeedwlirlprY4ESu44Blhb4fWGDi/j+aNNwK4S4eEc=";
        };
      };

      triple = {
        "x86_64-linux" = "x86_64-unknown-linux-musl";
        "aarch64-linux" = "aarch64-unknown-linux-musl";
      };

      tag = "v${release.version}";

      tarball = pkgs.fetchurl {
        url = "https://github.com/sshine/hashpinner/releases/download/${tag}/hashpinner-${tag}-${triple.${system}}.tar.gz";
        hash = release.hashes.${system};
      };
    in
    lib.optionalAttrs (release.hashes ? ${system}) {
      packages.hashpinner-bin =
        pkgs.runCommand "hashpinner-bin-${release.version}"
          {
            nativeBuildInputs = [ pkgs.makeWrapper ];

            meta = {
              description = "Check, pin and bump SHA-pinned GitHub/Forgejo Actions references (prebuilt)";
              mainProgram = "hashpinner";
              license = with lib.licenses; [
                mit
                asl20
              ];
              platforms = lib.attrNames release.hashes;
              sourceProvenance = [ lib.sourceTypes.binaryNativeCode ];
            };
          }
          ''
            tar -xzf ${tarball}
            install -Dm755 hashpinner $out/bin/hashpinner

            # The tag resolver shells out, the same reason the source build wraps it.
            wrapProgram $out/bin/hashpinner \
              --prefix PATH : ${lib.makeBinPath [ pkgs.git ]}
          '';
    };
}
