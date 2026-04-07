{
  lib,
  stdenv,
  fetchurl,
  autoPatchelfHook,
  zlib,
  python3,
  version ? "1.93.0.0",
}:

let
  hostTriple =
    {
      x86_64-linux = "x86_64-unknown-linux-gnu";
      aarch64-linux = "aarch64-unknown-linux-gnu";
      x86_64-darwin = "x86_64-apple-darwin";
      aarch64-darwin = "aarch64-apple-darwin";
    }
    .${stdenv.hostPlatform.system}
      or (throw "xtensa-rust-toolchain: unsupported host ${stdenv.hostPlatform.system}");

  # Hashes obtained via `nix-prefetch-url` against the upstream tarballs.
  # Bumping `version` requires regenerating all three.
  rustHashes = {
    "1.93.0.0" = {
      "x86_64-unknown-linux-gnu" = "0smdzbyjlk1s9ni74kvf7jyydw33m6p3lyxdvq7dl8n9cjmlcvkb";
      "aarch64-unknown-linux-gnu" = "1qp6h6w8zw4dj7pq0kwb86x0wdfgkz8lh5530vlyhvr6xgmzm8xr";
    };
  };

  rustSrcHashes = {
    "1.93.0.0" = "1a78j0x6m6x8d6dm4hv8llssa865agmz89vfvjs5187bcdjwp47n";
  };

  rustTarball = fetchurl {
    url = "https://github.com/esp-rs/rust-build/releases/download/v${version}/rust-${version}-${hostTriple}.tar.xz";
    sha256 = rustHashes.${version}.${hostTriple};
  };

  rustSrcTarball = fetchurl {
    url = "https://github.com/esp-rs/rust-build/releases/download/v${version}/rust-src-${version}.tar.xz";
    sha256 = rustSrcHashes.${version};
  };
in
stdenv.mkDerivation {
  pname = "xtensa-rust-toolchain";
  inherit version;

  srcs = [ rustTarball rustSrcTarball ];

  nativeBuildInputs = [ autoPatchelfHook ];

  # rustc binaries link against libz and libstdc++; bindgen-loaded
  # libclang ships separately via esp-clang and is patched by IDF.
  buildInputs = [
    zlib
    stdenv.cc.cc.lib
    python3
  ];

  dontBuild = true;
  dontConfigure = true;
  sourceRoot = ".";

  unpackPhase = ''
    runHook preUnpack
    mkdir rust rust-src
    tar -xf ${rustTarball} -C rust --strip-components=1
    tar -xf ${rustSrcTarball} -C rust-src --strip-components=1
    runHook postUnpack
  '';

  installPhase = ''
    runHook preInstall
    mkdir -p "$out"
    pushd rust >/dev/null
    bash ./install.sh --prefix="$out" --disable-ldconfig --verbose
    popd >/dev/null
    pushd rust-src >/dev/null
    bash ./install.sh --prefix="$out" --disable-ldconfig --verbose
    popd >/dev/null
    runHook postInstall
  '';

  # The toolchain is fully self-contained; do not let stripping touch
  # the precompiled rust-std artifacts.
  dontStrip = true;

  meta = with lib; {
    description = "Espressif's Xtensa-aware fork of Rust (rustc, cargo, rust-src) for ESP32/S2/S3";
    homepage = "https://github.com/esp-rs/rust-build";
    license = with licenses; [ mit asl20 ];
    platforms = [ "x86_64-linux" "aarch64-linux" "x86_64-darwin" "aarch64-darwin" ];
  };
}
