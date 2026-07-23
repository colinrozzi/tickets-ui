{
  description = "tickets-ui: Theater-native web UI on top of tickets-acceptor";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    crane.url = "github:ipetkov/crane";

    theater = {
      # packr 0.11.0 plain-build model (composition retired). theater main HEAD
      # PR #149 (73a4540b) — growable memory; plain cdylib actors load directly.
      url = "github:colinrozzi/theater/73a4540b";
      inputs.nixpkgs.follows = "nixpkgs";
      inputs.rust-overlay.follows = "rust-overlay";
      inputs.crane.follows = "crane";
    };
  };

  outputs = { self, nixpkgs, flake-utils, rust-overlay, crane, theater }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs { inherit system overlays; };

        rustToolchain = pkgs.rust-bin.stable.latest.default.override {
          targets = [ "wasm32-unknown-unknown" ];
        };

        craneLib = (crane.mkLib pkgs).overrideToolchain rustToolchain;

        src = pkgs.lib.cleanSourceWith {
          src = ./.;
          filter = path: type:
            (pkgs.lib.hasSuffix ".rs" path) ||
            (pkgs.lib.hasSuffix ".toml" path) ||
            (pkgs.lib.hasSuffix ".lock" path) ||
            (pkgs.lib.hasSuffix ".css" path) ||
            (pkgs.lib.hasSuffix ".html" path) ||
            (type == "directory");
        };

        # Plain self-contained link flags (packr 0.11.0; composition retired).
        # setup_guest!() links dlmalloc into the cdylib, so it exports its own
        # GROWABLE memory + __pack_alloc/__pack_free and imports only host
        # theater:simple/*. The resulting `nix build` .wasm is DIRECTLY loadable
        # — no compose step. crane does NOT reliably honor the repo
        # .cargo/config.toml, so pass the same two flags via
        # CARGO_ENCODED_RUSTFLAGS (0x1f-separated).
        plainSep = builtins.fromJSON "\"\\u001f\"";
        plainRustflags = builtins.concatStringsSep plainSep [
          "-C" "link-arg=--export-memory"
          "-C" "link-arg=--no-entry"
        ];

        commonArgs = {
          inherit src;
          pname = "tickets-ui";
          version = "0.1.0";
          cargoExtraArgs = "--target wasm32-unknown-unknown";
          CARGO_BUILD_TARGET = "wasm32-unknown-unknown";
          CARGO_ENCODED_RUSTFLAGS = plainRustflags;
          doCheck = false;
        };

        # Single buildPackage pass (no buildDepsOnly) — keeps the guest link
        # flags applied uniformly to the whole build. Cheap for a small actor.
        cargoArtifacts = null;

        theaterBin = theater.packages.${system}.default;

      in {
        packages.default = craneLib.buildPackage (commonArgs // {
          inherit cargoArtifacts;
          installPhaseCommand = ''
            mkdir -p $out
            cp target/wasm32-unknown-unknown/release/tickets_ui.wasm $out/
          '';
        });

        packages.theater = theaterBin;

        # nix run .#release — explicit version-stamping ceremony.
        # Creates a release tag (release-YYYYMMDD-<sha7>) on the current
        # HEAD and pushes it. The push triggers .github/workflows/release.yml
        # (when authored), which builds the wasm + uploads it to the GH
        # release. Mirrors tickets/inbox.
        packages.release = pkgs.writeShellScriptBin "tickets-ui-release" ''
          set -e
          BRANCH=$(${pkgs.git}/bin/git rev-parse --abbrev-ref HEAD)
          if [ "$BRANCH" != "main" ]; then
            echo "release: refusing to tag a non-main branch (current: $BRANCH)" >&2
            exit 1
          fi
          if ! ${pkgs.git}/bin/git diff --quiet HEAD 2>/dev/null || \
             ! ${pkgs.git}/bin/git diff --cached --quiet 2>/dev/null; then
            echo "release: refusing to tag with a dirty working tree" >&2
            exit 1
          fi
          DATE=$(date +%Y%m%d)
          SHA=$(${pkgs.git}/bin/git rev-parse --short=7 HEAD)
          TAG="release-$DATE-$SHA"
          if ${pkgs.git}/bin/git rev-parse "$TAG" >/dev/null 2>&1; then
            echo "release: tag $TAG already exists" >&2
            exit 1
          fi
          ${pkgs.git}/bin/git tag "$TAG"
          ${pkgs.git}/bin/git push origin "$TAG"
          echo "release: tagged + pushed $TAG"
          echo "release: CI will build + create the GH release at"
          echo "  https://github.com/colinrozzi/tickets-ui/releases/tag/$TAG"
        '';

        packages.clippy = craneLib.cargoClippy (commonArgs // {
          inherit cargoArtifacts;
          cargoClippyExtraArgs = "--target wasm32-unknown-unknown -- -D warnings";
        });

        packages.fmt = craneLib.cargoFmt {
          inherit src;
          pname = "tickets-ui";
          version = "0.1.0";
        };

        devShells.default = craneLib.devShell {
          # wasm-tools for the self-contained verify (host-only imports).
          # No binaryen — packr 0.11.0 retired the wasm-merge compose step.
          packages = [ rustToolchain theaterBin pkgs.ripgrep pkgs.wasm-tools ];
          shellHook = ''
            echo "tickets-ui dev environment"
            echo "  cargo build --release --target wasm32-unknown-unknown"
            echo "  theater spawn ui/manifest.toml"
          '';
        };
      });
}
