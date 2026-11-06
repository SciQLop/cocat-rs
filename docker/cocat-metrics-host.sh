#!/bin/bash
# On the sciqlop host, as root: serve the cocat dev server's metrics over HTTPS on port 9187
# to the cluster nodes only (VictoriaMetrics scrapes them), with a bearer token.
#  - token: generated once into ~sciqlop/.config/cocat-rs-dev/metrics.env (mode 600), read by
#    update-cocat-rs-dev; the cluster side copies it from there into its Secret.
#  - nginx: a TLS listener reusing the site's Let's Encrypt certificate, cluster subnet only,
#    proxying to the app's private metrics address 127.0.0.1:9188.
#  - firewalld: port 9187 open to 129.104.6.0/24 only.
set -euo pipefail

ENV=/home/sciqlop/.config/cocat-rs-dev/metrics.env
if [[ ! -f $ENV ]]; then
    umask 077
    printf 'COCAT_METRICS_TOKEN=%s\n' "$(openssl rand -hex 32)" > "$ENV"
    chown sciqlop:sciqlop "$ENV"
    chmod 600 "$ENV"
    echo "token written to $ENV"
fi

CONF=/etc/nginx/conf.d/cocat-metrics.conf
if [[ ! -f $CONF ]]; then
    cat > "$CONF" <<'NGINX'
# Prometheus metrics of the cocat dev server (cocat-rs-dev), for the cluster's
# VictoriaMetrics only: TLS with the site certificate, cluster subnet, bearer token checked
# by the app. The app itself listens on 127.0.0.1:9188.
server {
    listen 9187 ssl;
    server_name sciqlop.lpp.polytechnique.fr;
    ssl_certificate /etc/letsencrypt/live/sciqlop.lpp.polytechnique.fr/fullchain.pem;
    ssl_certificate_key /etc/letsencrypt/live/sciqlop.lpp.polytechnique.fr/privkey.pem;
    access_log /var/log/nginx/cocat-metrics.access.log;

    allow 129.104.6.0/24;
    deny all;

    location = /metrics {
        proxy_pass http://127.0.0.1:9188/metrics;
        proxy_set_header Authorization $http_authorization;
    }
    location / {
        return 404;
    }
}
NGINX
    echo "nginx listener written to $CONF"
fi
nginx -t
systemctl reload nginx

RULE='rule family="ipv4" source address="129.104.6.0/24" port port="9187" protocol="tcp" accept'
if ! firewall-cmd --permanent --query-rich-rule="$RULE" >/dev/null; then
    firewall-cmd --permanent --add-rich-rule="$RULE"
    firewall-cmd --reload
fi
echo DONE
