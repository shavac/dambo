(function () {
    const invoke = window.__TAURI__?.core?.invoke || window.__TAURI__?.invoke;
    const listen = window.__TAURI__?.event?.listen;
    const refreshBtn = document.getElementById('refresh-btn');
    const qrContainer = document.getElementById('qr-container');
    const statusText = document.getElementById('status-text');
    let dict = {};

    function tr(key, fallback) {
        return dict[key] || fallback;
    }

    function applyLocale(payload) {
        if (!payload) return;
        if (payload.locale) {
            document.documentElement.lang = payload.locale;
        }
        if (payload.messages) {
            dict = payload.messages;
        }
        document.title = tr('qr.window_title', 'Bilibili QR Sign In');
        document.querySelectorAll('[data-i18n]').forEach((el) => {
            const key = el.getAttribute('data-i18n');
            if (key && dict[key]) {
                el.textContent = dict[key];
            }
        });
    }

    function renderQrSvg(svgMarkup) {
        if (!qrContainer || typeof svgMarkup !== 'string' || !svgMarkup.trim()) return;
        const img = document.createElement('img');
        img.className = 'qr-img';
        img.alt = tr('qr.header', 'QR Code');
        img.src = `data:image/svg+xml;charset=utf-8,${encodeURIComponent(svgMarkup)}`;
        qrContainer.replaceChildren(img);
    }

    async function initQrWindow() {
        if (invoke) {
            invoke('get_i18n').then(applyLocale).catch(console.error);
        }
        if (listen) {
            await listen('i18n-updated', (event) => applyLocale(event.payload));
            await listen('qr_code_updated', (event) => {
                const data = event.payload;
                if (data && data.svg) {
                    renderQrSvg(data.svg);
                }
                if (data && data.message && statusText) {
                    statusText.textContent = data.message;
                }
                if (refreshBtn) {
                    refreshBtn.style.display = data && data.show_refresh ? 'inline-block' : 'none';
                }
            });
            await listen('qr_login_success', () => {
                if (statusText) {
                    statusText.textContent = tr('qr.success', 'Signed in, closing...');
                }
                if (refreshBtn) refreshBtn.style.display = 'none';
            });
        }

        refreshBtn?.addEventListener('click', () => {
            refreshBtn.style.display = 'none';
            if (statusText) {
                statusText.textContent = tr('qr.refreshing', 'Refreshing QR code...');
            }
            invoke?.('refresh_qr_code').catch(console.error);
        });

        invoke?.('refresh_qr_code').catch(console.error);
    }

    document.addEventListener('DOMContentLoaded', initQrWindow);
})();
