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
mkdir -p %{buildroot}%{_bindir}
cp rpxy %{buildroot}%{_bindir}/
mkdir -p %{buildroot}%{_sysconfdir}/systemd/system
cp rpxy.service %{buildroot}%{_sysconfdir}/systemd/system/
mkdir -p %{buildroot}%{_docdir}/rpxy
cp LICENSE %{buildroot}%{_docdir}/rpxy/
cp README.md %{buildroot}%{_docdir}/rpxy/

%clean
rm -rf %{buildroot}

%files
%license %{_docdir}/rpxy/LICENSE
%doc %{_docdir}/rpxy/README.md
%{_bindir}/rpxy
%{_sysconfdir}/systemd/system/rpxy.service

%post
systemctl daemon-reload
systemctl enable rpxy

%preun
systemctl stop rpxy

%postun
systemctl disable rpxy