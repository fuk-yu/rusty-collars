{
  description = "ESP32 collar controller firmware in Rust (fully reproducible toolchain)";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    nixpkgs-esp-dev.url = "github:mirrexagon/nixpkgs-esp-dev";
    qemu-espressif.url = "github:SFrijters/nix-qemu-espressif";
  };

  outputs =
    { self
    , nixpkgs
    , flake-utils
    , nixpkgs-esp-dev
    , qemu-espressif
    ,
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = import nixpkgs { inherit system; };

        # Single source of truth: parse Cargo.toml so the IDF version we
        # build against can never drift from what esp-idf-sys expects.
        cargoToml = builtins.fromTOML (builtins.readFile ./Cargo.toml);
        idfVersion = cargoToml.package.metadata.esp-idf-sys.esp_idf_version;

        # Override nixpkgs-esp-dev's esp-idf-full to the rev pinned in
        # Cargo.toml. The hash is for fetchFromGitHub with submodules and
        # must be regenerated when bumping idfVersion.
        espIdf = nixpkgs-esp-dev.packages.${system}.esp-idf-full.override {
          rev = idfVersion;
          sha256 = "sha256-sV/eL3jRG9GdaQNByBypmH5ZKmZoOnWCEY1ABySIeac=";
        };

        # Espressif's Xtensa-aware fork of Rust + matching rust-src,
        # autopatchelfed for /nix/store. Replaces espup entirely.
        xtensaRust = pkgs.callPackage ./nix/xtensa-rust-toolchain.nix { };

        # Espressif's libclang.so distribution for bindgen, since
        # mainline LLVM still does not enable the Xtensa target and
        # nixpkgs-esp-dev's esp-clang ships only the cross-compiler.
        # Pinned to libxml2_13 because libLLVM 18.1 was linked against
        # libxml2.so.2 (pre-2.13 SONAME bump).
        xtensaEspLibclang = pkgs.callPackage ./nix/xtensa-esp-libclang.nix {
          libxml2 = pkgs.libxml2_13;
        };
      in
      {
        packages = {
          inherit espIdf xtensaRust xtensaEspLibclang;
          default = espIdf;
        };

        devShells.default = pkgs.mkShell {
          packages = [
            espIdf

            # rustup is the cargo/rustc dispatcher. The actual toolchain
            # binaries are NOT on PATH directly — instead the shellHook
            # symlinks $RUSTUP_HOME/toolchains/esp to the Nix-built
            # xtensaRust derivation, and rustup's wrapper transparently
            # routes both `cargo` and `cargo +esp` to it. We never let
            # rustup install anything from the network.
            pkgs.rustup

            # Host-side helpers (no toolchain installation responsibilities).
            pkgs.espflash
            pkgs.ldproxy
            pkgs.cmake
            pkgs.ninja
            pkgs.pkg-config
            pkgs.openssl.dev
            pkgs.nodejs
            pkgs.git
            pkgs.python3

            qemu-espressif.packages.${system}.qemu-espressif
          ];

          shellHook = ''
            set -eu
            export PROJECT_ROOT="$PWD"

            # Sanity-check that Cargo.toml and the flake-pinned IDF agree.
            wanted="${idfVersion}"
            actual="$(cat "${espIdf}/version.txt" 2>/dev/null || echo unknown)"
            if [ "$actual" != "$wanted" ]; then
              echo "ESP-IDF version mismatch: Cargo.toml wants $wanted but flake provides $actual" >&2
            fi

            # rust-toolchain.toml says channel = "esp", which makes cargo
            # exec rustup. Seed a project-local RUSTUP_HOME so rustup finds
            # the Nix-built toolchain at $RUSTUP_HOME/toolchains/esp without
            # ever calling out to the network. Note: we deliberately do NOT
            # put ${xtensaRust}/bin on PATH — rustup is the only dispatcher,
            # so both `cargo` and `cargo +esp` route through it.
            export RUSTUP_HOME="$PROJECT_ROOT/.rustup"
            export CARGO_HOME="$PROJECT_ROOT/.cargo-home"
            mkdir -p "$RUSTUP_HOME/toolchains" "$CARGO_HOME/bin"
            ln -sfn "${xtensaRust}" "$RUSTUP_HOME/toolchains/esp"
            export PATH="$CARGO_HOME/bin:$PATH"

            # embuild's "fromenv" mode: trust IDF_PATH and friends from
            # the environment. nixpkgs-esp-dev's setup-hook already
            # exported IDF_PATH / IDF_TOOLS_PATH / IDF_PYTHON_ENV_PATH
            # before this shellHook runs.
            export ESP_IDF_TOOLS_INSTALL_DIR=fromenv

            # bindgen (used by esp-idf-sys) needs Espressif's libclang.so.
            # nixpkgs-esp-dev's esp-clang ships only the cross-compiler
            # frontend, so we provide libclang separately via the
            # xtensa-esp-libclang derivation.
            export LIBCLANG_PATH="${xtensaEspLibclang}/lib"

            # esp-idf-sys's build script (build/common.rs:setup_clang_env)
            # silently clobbers LIBCLANG_PATH if ~/.espup/esp-clang is a
            # symlink whose target directory exists. On hosts that ever
            # had a global espup install, that symlink can dangle into a
            # broken ~/.rustup tree and bindgen will try to dlopen a
            # libclang from there with unsatisfied libstdc++ deps. Tell
            # esp-idf-sys to skip the espup discovery entirely so our
            # /nix/store LIBCLANG_PATH wins unconditionally.
            export ESP_IDF_ESPUP_CLANG_SYMLINK=ignore
            set +eu
          '';
        };
      }
    );
}
