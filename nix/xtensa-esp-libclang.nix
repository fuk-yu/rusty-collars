{
  lib,
  stdenv,
  fetchurl,
  autoPatchelfHook,
  zlib,
  libxml2,
  ncurses,
  version ? "18.1.2_20240912",
}:

# Espressif's libclang.so distribution. Required by bindgen (used by
# esp-idf-sys) because mainline LLVM still does not enable the Xtensa
# target, so a stock libclang cannot parse esp32 headers. nixpkgs-esp-dev
# packages only the cross-compiler frontend (`bin/clang`) — this
# derivation provides the complementary host libclang.

let
  hostTriple =
    {
      x86_64-linux = "x86_64-linux-gnu";
      aarch64-linux = "aarch64-linux-gnu";
    }
    .${stdenv.hostPlatform.system}
      or (throw "xtensa-esp-libclang: unsupported host ${stdenv.hostPlatform.system}");

  hashes = {
    "18.1.2_20240912" = {
      "x86_64-linux-gnu" = "1i85rm77wzzpijclrzqdd0zb94dnw383gfxxwzim2dkd3nsgrmng";
      "aarch64-linux-gnu" = "0zhvpmvn7ankxbqlkynb67klx8a0a8fwkfj1vb8ba9hs6fkzv7fl";
    };
  };

  src = fetchurl {
    url = "https://github.com/espressif/llvm-project/releases/download/esp-${version}/libs-clang-esp-${version}-${hostTriple}.tar.xz";
    sha256 = hashes.${version}.${hostTriple};
  };
in
stdenv.mkDerivation {
  pname = "xtensa-esp-libclang";
  inherit version src;

  nativeBuildInputs = [ autoPatchelfHook ];

  buildInputs = [
    zlib
    libxml2.out
    ncurses
    stdenv.cc.cc.lib
  ];

  dontConfigure = true;
  dontBuild = true;
  dontStrip = true;

  installPhase = ''
    runHook preInstall
    mkdir -p "$out"
    # The tarball already has its own top-level directory; cp into $out.
    cp -r * "$out/"
    runHook postInstall
  '';

  meta = with lib; {
    description = "Espressif's libclang.so build for use with bindgen on esp32 targets";
    homepage = "https://github.com/espressif/llvm-project";
    license = licenses.asl20;
    platforms = [ "x86_64-linux" "aarch64-linux" ];
  };
}
