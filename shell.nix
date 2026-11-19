{ pkgs ? import <nixpkgs> { } }:
pkgs.mkShell {
  nativeBuildInputs = with pkgs; [ pkg-config clang rustc cargo wrapGAppsHook4 ];
  buildInputs = with pkgs; [ pipewire dbus gtk4 glib gsettings-desktop-schemas adwaita-icon-theme ];
  LIBCLANG_PATH = "${pkgs.libclang.lib}/lib";
}
