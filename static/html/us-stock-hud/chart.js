(function (root, factory) {
    if (typeof module === 'object' && module.exports) {
        module.exports = factory();
    } else {
        root.UsStockChart = factory();
    }
})(typeof globalThis !== 'undefined' ? globalThis : window, function () {
    'use strict';

    const SVG_NS = 'http://www.w3.org/2000/svg';

    function formatAxisPrice(val) {
        const n = Number(val || 0);
        if (!Number.isFinite(n)) return '--';
        if (n >= 1000) return n.toFixed(1);
        if (n < 10) return n.toFixed(3);
        return n.toFixed(2);
    }

    function hexToRgba(hex, alpha) {
        const clean = String(hex || '').replace('#', '').trim();
        if (clean.length === 3) {
            const r = parseInt(clean[0] + clean[0], 16);
            const g = parseInt(clean[1] + clean[1], 16);
            const b = parseInt(clean[2] + clean[2], 16);
            return 'rgba(' + r + ',' + g + ',' + b + ',' + alpha + ')';
        }
        if (clean.length === 6) {
            const r = parseInt(clean.slice(0, 2), 16);
            const g = parseInt(clean.slice(2, 4), 16);
            const b = parseInt(clean.slice(4, 6), 16);
            if (Number.isFinite(r) && Number.isFinite(g) && Number.isFinite(b)) {
                return 'rgba(' + r + ',' + g + ',' + b + ',' + alpha + ')';
            }
        }
        return 'rgba(0, 230, 118, ' + alpha + ')';
    }

    function formatEtShortTime(tsMs) {
        try {
            return new Intl.DateTimeFormat('en-US', {
                timeZone: 'America/New_York',
                hour: '2-digit',
                minute: '2-digit',
                hour12: false,
            }).format(new Date(tsMs));
        } catch (_) {
            const d = new Date(tsMs);
            return String(d.getHours()).padStart(2, '0') + ':' + String(d.getMinutes()).padStart(2, '0');
        }
    }

    function drawRoundRectPath(ctx, x, y, w, h, r) {
        const radius = Math.min(r, w / 2, h / 2);
        ctx.beginPath();
        ctx.moveTo(x + radius, y);
        ctx.lineTo(x + w - radius, y);
        ctx.quadraticCurveTo(x + w, y, x + w, y + radius);
        ctx.lineTo(x + w, y + h - radius);
        ctx.quadraticCurveTo(x + w, y + h, x + w - radius, y + h);
        ctx.lineTo(x + radius, y + h);
        ctx.quadraticCurveTo(x, y + h, x, y + h - radius);
        ctx.lineTo(x, y + radius);
        ctx.quadraticCurveTo(x, y, x + radius, y);
        ctx.closePath();
    }

    /**
     * Render intraday canvas chart with previous-close line and Room Average Cost dashed line.
     */
    function drawIntradayChart(canvas, opts) {
        if (!canvas || typeof canvas.getContext !== 'function') return;
        const rect = canvas.getBoundingClientRect();
        const cssW = Math.max(300, Math.floor(rect.width || canvas.clientWidth || 720));
        const cssH = Math.max(180, Math.floor(rect.height || canvas.clientHeight || 360));
        const dpr = Math.min(2, window.devicePixelRatio || 1);

        if (canvas.width !== Math.floor(cssW * dpr) || canvas.height !== Math.floor(cssH * dpr)) {
            canvas.width = Math.floor(cssW * dpr);
            canvas.height = Math.floor(cssH * dpr);
        }

        const ctx = canvas.getContext('2d');
        ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
        ctx.clearRect(0, 0, cssW, cssH);

        const padL = 16;
        const padR = 88;
        const padT = 18;
        const padB = 30;
        const plotW = Math.max(80, cssW - padL - padR);
        const plotH = Math.max(60, cssH - padT - padB);

        const quote = opts && opts.quote ? opts.quote : null;
        const series = opts && Array.isArray(opts.series) ? opts.series : [];
        const roomStats = opts && opts.roomStats ? opts.roomStats : null;
        const colorUp = (opts && opts.colorUp) || '#00E676';
        const colorDown = (opts && opts.colorDown) || '#FF3B30';
        const colorGold = (opts && opts.colorGold) || '#F59E0B';

        const points = series.length >= 2
            ? series
            : (quote && quote.c > 0
                ? [
                    { t: Date.now() - 3600000, p: quote.pc > 0 ? quote.pc : quote.c },
                    { t: Date.now(), p: quote.c },
                ]
                : []);

        if (points.length < 2) {
            ctx.fillStyle = 'rgba(148, 163, 184, 0.7)';
            ctx.font = '600 15px system-ui, -apple-system, sans-serif';
            ctx.textAlign = 'center';
            ctx.fillText('正在同步实时分时数据…', cssW / 2, cssH / 2);
            return;
        }

        const latestP = points[points.length - 1].p;
        const prevClose = quote && quote.pc > 0 ? quote.pc : points[0].p;
        const isUp = latestP >= prevClose;
        const lineColor = isUp ? colorUp : colorDown;

        let minP = Infinity;
        let maxP = -Infinity;
        for (let i = 0; i < points.length; i++) {
            const p = Number(points[i].p);
            if (p < minP) minP = p;
            if (p > maxP) maxP = p;
        }
        if (prevClose > 0) {
            const relDiff = Math.abs(prevClose - latestP) / Math.max(latestP, 0.01);
            if (relDiff < 0.15) {
                minP = Math.min(minP, prevClose);
                maxP = Math.max(maxP, prevClose);
            }
        }

        const avgCost = roomStats && Number(roomStats.overallAvgCost) > 0
            ? Number(roomStats.overallAvgCost)
            : 0;
        let avgWithinComfortRange = false;
        if (avgCost > 0) {
            const distRatio = Math.abs(avgCost - latestP) / Math.max(latestP, 0.01);
            if (distRatio <= 0.18) {
                avgWithinComfortRange = true;
                minP = Math.min(minP, avgCost);
                maxP = Math.max(maxP, avgCost);
            }
        }

        const rawSpan = maxP - minP;
        const padSpan = rawSpan > 0 ? rawSpan * 0.16 : Math.max(latestP * 0.008, 0.25);
        minP -= padSpan;
        maxP += padSpan;
        const span = Math.max(maxP - minP, 0.0001);

        function yForPrice(p) {
            return padT + ((maxP - p) / span) * plotH;
        }
        function xForIndex(idx) {
            return padL + (idx / (points.length - 1)) * plotW;
        }

        // 1. Horizontal & vertical subtle grid
        ctx.lineWidth = 1;
        ctx.strokeStyle = 'rgba(148, 163, 184, 0.10)';
        ctx.fillStyle = 'rgba(148, 163, 184, 0.72)';
        ctx.font = '600 14px "SF Mono", Menlo, Consolas, monospace';
        ctx.textAlign = 'left';
        ctx.textBaseline = 'middle';

        const gridRows = 4;
        for (let g = 0; g <= gridRows; g++) {
            const ratio = g / gridRows;
            const y = padT + ratio * plotH;
            const priceLabel = maxP - ratio * span;
            ctx.beginPath();
            ctx.moveTo(padL, y);
            ctx.lineTo(padL + plotW, y);
            ctx.stroke();
            ctx.fillText(formatAxisPrice(priceLabel), padL + plotW + 8, y);
        }

        // X-axis ET timestamps
        ctx.textAlign = 'center';
        ctx.textBaseline = 'top';
        const timeTicks = [0, Math.floor((points.length - 1) * 0.33), Math.floor((points.length - 1) * 0.66), points.length - 1];
        for (let t = 0; t < timeTicks.length; t++) {
            const idx = timeTicks[t];
            const x = xForIndex(idx);
            ctx.beginPath();
            ctx.moveTo(x, padT);
            ctx.lineTo(x, padT + plotH);
            ctx.stroke();
            const label = formatEtShortTime(points[idx].t);
            const clampedX = Math.max(padL + 24, Math.min(padL + plotW - 20, x));
            ctx.fillText(label + ' ET', clampedX, padT + plotH + 7);
        }

        // 2. Previous Close Reference Dashed Line
        if (prevClose > 0 && prevClose >= minP && prevClose <= maxP) {
            const pcY = yForPrice(prevClose);
            ctx.save();
            ctx.setLineDash([4, 4]);
            ctx.strokeStyle = 'rgba(148, 163, 184, 0.45)';
            ctx.lineWidth = 1.2;
            ctx.beginPath();
            ctx.moveTo(padL, pcY);
            ctx.lineTo(padL + plotW, pcY);
            ctx.stroke();
            ctx.restore();

            ctx.fillStyle = 'rgba(148, 163, 184, 0.85)';
            ctx.font = '600 14px system-ui, sans-serif';
            ctx.textAlign = 'right';
            ctx.textBaseline = 'bottom';
            ctx.fillText('昨收 ' + formatAxisPrice(prevClose), padL + plotW - 6, pcY - 3);
        }

        // 3. Area Gradient + Intraday Line
        const grad = ctx.createLinearGradient(0, padT, 0, padT + plotH);
        grad.addColorStop(0, hexToRgba(lineColor, 0.30));
        grad.addColorStop(0.75, hexToRgba(lineColor, 0.06));
        grad.addColorStop(1, hexToRgba(lineColor, 0.0));

        ctx.beginPath();
        for (let i = 0; i < points.length; i++) {
            const x = xForIndex(i);
            const y = yForPrice(points[i].p);
            if (i === 0) ctx.moveTo(x, y);
            else ctx.lineTo(x, y);
        }
        ctx.lineTo(xForIndex(points.length - 1), padT + plotH);
        ctx.lineTo(xForIndex(0), padT + plotH);
        ctx.closePath();
        ctx.fillStyle = grad;
        ctx.fill();

        ctx.beginPath();
        for (let i = 0; i < points.length; i++) {
            const x = xForIndex(i);
            const y = yForPrice(points[i].p);
            if (i === 0) ctx.moveTo(x, y);
            else ctx.lineTo(x, y);
        }
        ctx.strokeStyle = lineColor;
        ctx.lineWidth = 2.4;
        ctx.lineJoin = 'round';
        ctx.lineCap = 'round';
        ctx.stroke();

        // 4. Room Average Holding Cost Dashed Line ("全服玩家平均持仓成本价")
        if (avgCost > 0) {
            const rawAvgY = yForPrice(avgCost);
            const clampedAvgY = avgWithinComfortRange
                ? rawAvgY
                : (avgCost > maxP ? padT + 14 : padT + plotH - 14);
            const arrowPrefix = !avgWithinComfortRange
                ? (avgCost > maxP ? '▲ ' : '▼ ')
                : '';

            ctx.save();
            ctx.setLineDash([7, 5]);
            ctx.strokeStyle = colorGold;
            ctx.lineWidth = 1.8;
            ctx.beginPath();
            ctx.moveTo(padL, clampedAvgY);
            ctx.lineTo(padL + plotW, clampedAvgY);
            ctx.stroke();
            ctx.restore();

            const costText = arrowPrefix + '全服玩家平均持仓成本价 ' + '$' + formatAxisPrice(avgCost);
            ctx.font = '700 14px system-ui, -apple-system, sans-serif';
            const textW = ctx.measureText(costText).width;
            const pillW = textW + 18;
            const pillH = 26;
            const pillX = padL + plotW - pillW - 8;
            const pillY = Math.max(padT + 4, Math.min(padT + plotH - pillH - 4, clampedAvgY - pillH - 4));

            drawRoundRectPath(ctx, pillX, pillY, pillW, pillH, 6);
            ctx.fillStyle = 'rgba(17, 24, 39, 0.92)';
            ctx.fill();
            ctx.strokeStyle = colorGold;
            ctx.lineWidth = 1.2;
            ctx.stroke();

            ctx.fillStyle = colorGold;
            ctx.textAlign = 'left';
            ctx.textBaseline = 'middle';
            ctx.fillText(costText, pillX + 9, pillY + pillH / 2);
        }

        // 5. Latest Point Glow + Right-Axis Current Price Pill
        const lastX = xForIndex(points.length - 1);
        const lastY = yForPrice(latestP);

        ctx.beginPath();
        ctx.arc(lastX, lastY, 7, 0, Math.PI * 2);
        ctx.fillStyle = hexToRgba(lineColor, 0.25);
        ctx.fill();

        ctx.beginPath();
        ctx.arc(lastX, lastY, 3.6, 0, Math.PI * 2);
        ctx.fillStyle = lineColor;
        ctx.fill();

        const tagY = Math.max(padT + 12, Math.min(padT + plotH - 12, lastY));
        drawRoundRectPath(ctx, padL + plotW + 4, tagY - 12, padR - 8, 24, 5);
        ctx.fillStyle = lineColor;
        ctx.fill();

        ctx.fillStyle = '#060A12';
        ctx.font = '700 14px "SF Mono", Menlo, Consolas, monospace';
        ctx.textAlign = 'center';
        ctx.textBaseline = 'middle';
        ctx.fillText(formatAxisPrice(latestP), padL + plotW + 4 + (padR - 8) / 2, tagY);
    }

    /**
     * Render compact SVG sparkline via safe SVG DOM nodes.
     */
    function renderSparkline(svgEl, prices, isUp, colorUp, colorDown) {
        if (!svgEl) return;
        const w = 96;
        const h = 30;
        svgEl.setAttribute('viewBox', '0 0 ' + w + ' ' + h);
        svgEl.setAttribute('preserveAspectRatio', 'none');

        const pts = Array.isArray(prices) && prices.length >= 2
            ? prices
            : [100, 100];
        let minP = Infinity;
        let maxP = -Infinity;
        for (let i = 0; i < pts.length; i++) {
            const p = Number(pts[i]);
            if (p < minP) minP = p;
            if (p > maxP) maxP = p;
        }
        const span = maxP - minP || Math.max(minP * 0.01, 0.01);
        const coords = [];
        for (let i = 0; i < pts.length; i++) {
            const x = (i / (pts.length - 1)) * w;
            const y = 3 + ((maxP - Number(pts[i])) / span) * (h - 6);
            coords.push(x.toFixed(1) + ',' + y.toFixed(1));
        }

        const strokeColor = isUp ? (colorUp || '#00E676') : (colorDown || '#FF3B30');
        const fillCoords = ['0,' + h].concat(coords, [w + ',' + h]).join(' ');

        const polygon = document.createElementNS(SVG_NS, 'polygon');
        polygon.setAttribute('points', fillCoords);
        polygon.setAttribute('fill', hexToRgba(strokeColor, 0.16));

        const polyline = document.createElementNS(SVG_NS, 'polyline');
        polyline.setAttribute('points', coords.join(' '));
        polyline.setAttribute('fill', 'none');
        polyline.setAttribute('stroke', strokeColor);
        polyline.setAttribute('stroke-width', '1.8');
        polyline.setAttribute('stroke-linecap', 'round');
        polyline.setAttribute('stroke-linejoin', 'round');

        svgEl.replaceChildren(polygon, polyline);
    }

    return {
        drawIntradayChart: drawIntradayChart,
        renderSparkline: renderSparkline,
    };
});
