with import <nixpkgs> {};
mkShell {
  packages = [ pkgs.maven ];
  NIX_LD_LIBRARY_PATH = lib.makeLibraryPath [
    pkgs.gcc
  ];
  NIX_LD = lib.fileContents "${stdenv.cc}/nix-support/dynamic-linker";
}
