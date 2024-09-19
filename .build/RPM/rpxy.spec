Name:           rpxy
Version:        @BUILD_VERSION@
Release:        1%{?dist}
Summary:        A simple and ultrafast reverse-proxy serving multiple domain names with TLS termination, written in Rust

License:        MIT
URL:            https://github.com/junkurihara/rust-rpxy
Source0:        @Source0@
BuildArch:      x86_64

Requires:       systemd

%description
This rpm installs rpxy into /usr/bin and sets up a systemd service.

# Prep section: Unpack the source
%prep
%autosetup

# Install section: Copy files to their destinations
%install
rm -rf %{buildroot}

# Create necessary directories
mkdir -p %{buildroot}%{_bindir}
mkdir -p %{buildroot}%{_sysconfdir}/systemd/system
mkdir -p %{buildroot}%{_sysconfdir}/rpxy/acme_registry
mkdir -p %{buildroot}%{_docdir}/rpxy

# Copy files
cp rpxy %{buildroot}%{_bindir}/
cp rpxy.service %{buildroot}%{_sysconfdir}/systemd/system/
cp config.toml %{buildroot}%{_sysconfdir}/rpxy/
cp LICENSE README.md %{buildroot}%{_docdir}/rpxy/

# Clean section: Remove buildroot
%clean
rm -rf %{buildroot}

# Pre-install script
%pre
# Create the rpxy user if it does not exist
if ! getent passwd rpxy >/dev/null; then
    useradd -r -s /sbin/nologin -d / -c "rpxy system user" rpxy
fi

# Post-install script
%post
# Set ownership of config file to rpxy user
chown -R rpxy:rpxy %{_sysconfdir}/rpxy

# Reload systemd, enable and start rpxy service
%systemd_post rpxy.service

# Pre-uninstall script
%preun
%systemd_preun rpxy.service

# Post-uninstall script
%postun
%systemd_postun_with_restart rpxy.service

# Only remove user and config on full uninstall
if [ $1 -eq 0 ]; then
    # Remove rpxy user
    userdel rpxy

    # Remove the configuration directory if it exists
    [ -d %{_sysconfdir}/rpxy ] && rm -rf %{_sysconfdir}/rpxy
fi

# Files section: List all files included in the package
%files
%license %{_docdir}/rpxy/LICENSE
%doc %{_docdir}/rpxy/README.md
%{_sysconfdir}/systemd/system/rpxy.service
%attr(755, rpxy, rpxy) %{_bindir}/rpxy
%attr(644, rpxy, rpxy) %config(noreplace) %{_sysconfdir}/rpxy/config.toml
