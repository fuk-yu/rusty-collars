{
  description = "ESP32 collar controller firmware in Rust";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    qemu-espressif.url = "github:SFrijters/nix-qemu-espressif";
  };

  outputs = { self, nixpkgs, qemu-espressif }:
    let
      system = "x86_64-linux";
      pkgs = import nixpkgs { inherit system; };
    in
    {
      devShells.${system}.default = pkgs.mkShell {
        packages = with pkgs; [
          rustup
          espup
          espflash
          ldproxy
          cmake
          ninja
          patchelf
          python3
          python3Packages.pip
          python3Packages.virtualenv
          git
          pkg-config
          openssl.dev

          # ESP32 QEMU emulator
          qemu-espressif.packages.${system}.qemu-espressif
        ];

        ESP_IDF_TOOLS_INSTALL_DIR = "fromenv";

        shellHook = ''
          export PROJECT_ROOT="$PWD"
          export CARGO_HOME="$PROJECT_ROOT/.cargo-home"
          export RUSTUP_HOME="$PROJECT_ROOT/.rustup"
          export ESPUP_EXPORT_FILE="$PROJECT_ROOT/export-esp.sh"
          export TOOLCHAIN_ALIAS_DIR="$PROJECT_ROOT/.toolchain-bin"
          export PATH="$CARGO_HOME/bin:$PATH"
          export LD_LIBRARY_PATH="${pkgs.lib.makeLibraryPath [ pkgs.stdenv.cc.cc.lib ]}:''${LD_LIBRARY_PATH:-}"
          mkdir -p "$CARGO_HOME" "$RUSTUP_HOME"

          # Repo-local espup export file adds the Xtensa Rust toolchain and esp-clang.
          [ -f "$ESPUP_EXPORT_FILE" ] && . "$ESPUP_EXPORT_FILE"

          "$PROJECT_ROOT/scripts/prepare-toolchain-env.sh"
          export PATH="$TOOLCHAIN_ALIAS_DIR:$PATH"

          if ! cargo +esp --version >/dev/null 2>&1; then
            echo ""
            echo "Repo-local ESP Rust toolchain not found."
            echo "  Run: ./scripts/bootstrap-toolchain.sh"
            echo "  Then reload direnv or open a new shell"
            echo ""
          fi

          # Point to the ESP-IDF clone managed by previous builds
          for _idf in "$PWD"/.embuild/espressif/esp-idf/v5.*; do
            [ -d "$_idf" ] || continue
            export IDF_PATH="$_idf"
            export PATH="$IDF_PATH/tools:$PATH"
            export IDF_TOOLS_PATH="$PWD/.embuild/espressif"
            # Python env
            for _pyenv in "$PWD"/.embuild/espressif/python_env/idf5.*; do
              [ -d "$_pyenv" ] && export PATH="$_pyenv/bin:$PATH" && export IDF_PYTHON_ENV_PATH="$_pyenv"
            done
            # Cross-compilers (use newest available version)
            for _dir in "$PWD"/.embuild/espressif/tools/xtensa-esp-elf/*/xtensa-esp-elf/bin; do
              [ -d "$_dir" ] && export PATH="$_dir:$PATH"
            done
            for _dir in "$PWD"/.embuild/espressif/tools/riscv32-esp-elf/*/riscv32-esp-elf/bin; do
              [ -d "$_dir" ] && export PATH="$_dir:$PATH"
            done
            break
          done
        '';
      };
    };
}
