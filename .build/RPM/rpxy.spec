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

%prep
%autosetup

%install
rm -rf %{buildroot}
# Copy binary
mkdir -p %{buildroot}%{_bindir}
cp rpxy %{buildroot}%{_bindir}/
# Create systemd service
mkdir -p %{buildroot}%{_sysconfdir}/systemd/system
cp rpxy.service %{buildroot}%{_sysconfdir}/systemd/system/
# Create config directory
mkdir -p %{buildroot}%{_sysconfdir}/rpxy/acme_registry
cp config.toml %{buildroot}%{_sysconfdir}/rpxy/
# Copy documentation
mkdir -p %{buildroot}%{_docdir}/rpxy
cp LICENSE %{buildroot}%{_docdir}/rpxy/
cp README.md %{buildroot}%{_docdir}/rpxy/

%clean
rm -rf %{buildroot}

%pre
# Create the rpxy user if it does not exist
if ! id rpxy >/dev/null 2>&1; then
    /usr/sbin/useradd -r -s /bin/false -d / -c "rpxy system user" rpxy
fi

%post
# Set ownership of config file to rpxy user
chown -R rpxy:rpxy %{_sysconfdir}/rpxy

# Reload systemd, enable and start rpxy service
systemctl daemon-reload
systemctl enable rpxy
if [ $1 -eq 1 ]; then
    systemctl start rpxy
fi

%preun
# Stop the service on uninstall or upgrade
if [ $1 -eq 0 ]; then
    systemctl stop rpxy
fi

%postun
# On uninstall, disable the service and reload systemd
if [ $1 -eq 0 ]; then
    systemctl disable rpxy
    systemctl daemon-reload
fi

# Remove rpxy user only if package is being completely removed (not upgraded)
if [ $1 -eq 0 ]; then
    # Check if the rpxy user exists before attempting to delete
    if id rpxy >/dev/null 2>&1; then
        /usr/sbin/userdel rpxy
    fi

    # Remove the configuration directory if it exists and is empty
    if [ -d %{_sysconfdir}/rpxy ]; then
        rm -rf %{_sysconfdir}/rpxy
    fi
fi

%files
%license %{_docdir}/rpxy/LICENSE
%doc %{_docdir}/rpxy/README.md
%{_sysconfdir}/systemd/system/rpxy.service
%attr(-, rpxy, rpxy) %{_bindir}/rpxy
%attr(-, rpxy, rpxy) %config(noreplace) %{_sysconfdir}/rpxy/config.toml
