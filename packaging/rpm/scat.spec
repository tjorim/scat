Name:           scat
Version:        %{scat_version}
Release:        %{scat_release}%{?dist}
Summary:        Standalone script catalog CLI and terminal UI
License:        LicenseRef-Unspecified
URL:            https://github.com/tjorim/scat
Source0:        payload.tar
ExclusiveArch:  x86_64
AutoReqProv:    no
%global debug_package %{nil}
%global __os_install_post %{nil}

%description
Static musl script catalog client. Catalogs and caches are managed separately.
No services, users, host security policy changes or install-time downloads.

%prep
%setup -q -c

%build
# The input executable is the existing release artifact; never rebuild it here.

%install
mkdir -p %{buildroot}
cp -a . %{buildroot}/

%files
%{_bindir}/scat
%dir %{_sysconfdir}/scat
%config(noreplace) %{_sysconfdir}/scat/scat.yaml
%{_datadir}/bash-completion/completions/scat
%{_datadir}/zsh/site-functions/_scat
%{_datadir}/fish/vendor_completions.d/scat.fish
%doc %{_docdir}/scat
