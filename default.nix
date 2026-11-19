{ pkgs ? import <nixpkgs> { } }:
pkgs.rustPlatform.buildRustPackage {
  pname = "wavectl";
  version = "0.1.0";
  src = pkgs.lib.cleanSourceWith {
    src = ./.;
    filter = path: type: !(builtins.elem (baseNameOf path) [ "target" ".git" "result" ]);
  };
  cargoLock.lockFile = ./Cargo.lock;
  nativeBuildInputs = with pkgs; [ pkg-config rustPlatform.bindgenHook wrapGAppsHook4 ];
  buildInputs = with pkgs; [ pipewire dbus gtk4 glib gsettings-desktop-schemas adwaita-icon-theme ];
  # Daemon and tray don't need GTK; wrapping them is harmless.
  postInstall = ''
    install -Dm644 packaging/wavectl.desktop $out/share/applications/wavectl.desktop
    mkdir -p $out/share/icons
    cp -r packaging/icons/hicolor $out/share/icons/
    # Tray icons for hosts that take an IconThemePath (GNOME's AppIndicator builds a
    # private theme from it and needs an index.theme). Kept out of share/icons so it
    # can't collide with hicolor-icon-theme in a profile.
    mkdir -p $out/share/wavectl/icons
    cp -r packaging/icons/hicolor $out/share/wavectl/icons/
    install -Dm644 packaging/tray-theme/index.theme $out/share/wavectl/icons/hicolor/index.theme
    # systemd user units, found via XDG_DATA_DIRS (profile/share/systemd/user)
    for u in wavectld wavectl-tray; do
      install -Dm644 packaging/$u.service $out/share/systemd/user/$u.service
      substituteInPlace $out/share/systemd/user/$u.service --replace-fail @out@ $out
    done
  '';
  meta.mainProgram = "wavectl";
}
