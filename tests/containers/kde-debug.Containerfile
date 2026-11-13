FROM localhost/shotput-test:kde
USER root
RUN dnf -y install --setopt=install_weak_deps=False gdb && dnf clean all
USER tester
