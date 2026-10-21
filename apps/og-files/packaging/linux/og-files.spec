# Packages the prebuilt release binary (built by CI); not a from-source spec.
# rpmbuild -bb --define "_version X.Y.Z" --define "_srcdir /path/to/stage" og-files.spec
Name:           og-files
Version:        %{_version}
Release:        1
Summary:        File manager with built-in viewers and editors
License:        MPL-2.0
URL:            https://github.com/OmegaGiven/og-files
Requires:       alsa-lib, libxkbcommon, vulkan-loader
Recommends:     mpv-libs, udisks2, rclone, fuse3
AutoReqProv:    yes

%global debug_package %{nil}
%global __strip /bin/true

%description
OG Files browses, previews and edits documents, PDFs, images, video, audio,
spreadsheets, archives and 3D models in one window.

%install
install -Dm755 %{_srcdir}/og-files %{buildroot}%{_bindir}/og-files
install -Dm644 %{_srcdir}/og-files.desktop %{buildroot}%{_datadir}/applications/og-files.desktop
install -Dm644 %{_srcdir}/og-files.png %{buildroot}%{_datadir}/icons/hicolor/512x512/apps/og-files.png
install -Dm644 %{_srcdir}/og-files.svg %{buildroot}%{_datadir}/icons/hicolor/scalable/apps/og-files.svg
install -Dm644 %{_srcdir}/com.omegagiven.ogfiles.metainfo.xml %{buildroot}%{_datadir}/metainfo/com.omegagiven.ogfiles.metainfo.xml
install -Dm644 %{_srcdir}/LICENSE %{buildroot}%{_datadir}/licenses/og-files/LICENSE

%files
%{_bindir}/og-files
%{_datadir}/applications/og-files.desktop
%{_datadir}/icons/hicolor/512x512/apps/og-files.png
%{_datadir}/icons/hicolor/scalable/apps/og-files.svg
%{_datadir}/metainfo/com.omegagiven.ogfiles.metainfo.xml
%license %{_datadir}/licenses/og-files/LICENSE
