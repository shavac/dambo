(() => {
  if (window.__DAMBO_OVERLAY_CTRL__) return;
  window.__DAMBO_OVERLAY_CTRL__ = true;

  const DRAG_TITLE = __DAMBO_DRAG_TITLE__;
  const PIN_TITLE = __DAMBO_PIN_TITLE__;
  const UNPIN_TITLE = __DAMBO_UNPIN_TITLE__;
  const CLOSE_TITLE = __DAMBO_CLOSE_TITLE__;

  function invokeWindowCmd(cmd, arg) {
    const api = window.__TAURI__?.window?.getCurrentWindow?.();
    if (cmd === 'start_dragging') {
      if (api && typeof api.startDragging === 'function') {
        return api.startDragging().catch(() => window.__TAURI_INTERNALS__?.invoke?.('plugin:window|start_dragging'));
      }
      return window.__TAURI_INTERNALS__?.invoke?.('plugin:window|start_dragging');
    }
    if (cmd === 'outer_position') {
      if (api && typeof api.outerPosition === 'function') {
        return api.outerPosition().catch(() => window.__TAURI_INTERNALS__?.invoke?.('plugin:window|outer_position'));
      }
      return window.__TAURI_INTERNALS__?.invoke?.('plugin:window|outer_position');
    }
    if (cmd === 'scale_factor') {
      if (api && typeof api.scaleFactor === 'function') {
        return api.scaleFactor().catch(() => window.__TAURI_INTERNALS__?.invoke?.('plugin:window|scale_factor'));
      }
      return window.__TAURI_INTERNALS__?.invoke?.('plugin:window|scale_factor');
    }
    if (cmd === 'set_position') {
      const x = Math.round(arg.x);
      const y = Math.round(arg.y);
      const PhysicalPositionCtor = window.__TAURI__?.window?.PhysicalPosition || window.__TAURI__?.dpi?.PhysicalPosition;
      if (api && typeof api.setPosition === 'function' && PhysicalPositionCtor) {
        return api.setPosition(new PhysicalPositionCtor(x, y)).catch(() =>
          window.__TAURI_INTERNALS__?.invoke?.('plugin:window|set_position', { value: { Physical: { x, y } } })
        );
      }
      return window.__TAURI_INTERNALS__?.invoke?.('plugin:window|set_position', { value: { Physical: { x, y } } });
    }
    if (cmd === 'is_always_on_top') {
      if (api && typeof api.isAlwaysOnTop === 'function') {
        return api.isAlwaysOnTop().catch(() => window.__TAURI_INTERNALS__?.invoke?.('plugin:window|is_always_on_top'));
      }
      return window.__TAURI_INTERNALS__?.invoke?.('plugin:window|is_always_on_top');
    }
    if (cmd === 'set_always_on_top') {
      const value = Boolean(arg);
      if (api && typeof api.setAlwaysOnTop === 'function') {
        return api.setAlwaysOnTop(value).catch(() => window.__TAURI_INTERNALS__?.invoke?.('plugin:window|set_always_on_top', { value }));
      }
      return window.__TAURI_INTERNALS__?.invoke?.('plugin:window|set_always_on_top', { value });
    }
    if (cmd === 'close') {
      if (api && typeof api.close === 'function') {
        return api.close().catch(() => {
          if (typeof api.destroy === 'function') return api.destroy();
          return window.__TAURI_INTERNALS__?.invoke?.('plugin:window|close');
        });
      }
      const p = window.__TAURI_INTERNALS__?.invoke?.('plugin:window|close');
      if (p && typeof p.catch === 'function') {
        return p.catch(() => window.__TAURI_INTERNALS__?.invoke?.('plugin:window|destroy'));
      }
      return p;
    }
  }

  function triggerDrag() {
    Promise.resolve(invokeWindowCmd('start_dragging')).catch((err) => {
      console.error('[dambo overlay] drag failed:', err);
    });
  }

  function triggerClose() {
    Promise.resolve(invokeWindowCmd('close')).catch((err) => {
      console.error('[dambo overlay] close failed:', err);
    });
  }

  function mount() {
    if (document.getElementById('dambo-overlay-host')) return;
    const host = document.createElement('div');
    host.id = 'dambo-overlay-host';
    host.style.cssText = 'position:fixed;inset:0;pointer-events:none;z-index:2147483647;background:transparent;';

    const shadow = host.attachShadow({ mode: 'open' });
    shadow.innerHTML = `
      <style>
        .dambo-root {
          position: fixed;
          inset: 0;
          pointer-events: none;
          user-select: none;
          -webkit-user-select: none;
        }
        .dambo-frame {
          position: fixed;
          inset: 0;
          border: 1.5px dashed rgba(255, 255, 255, 0.36);
          border-radius: 10px;
          box-sizing: border-box;
          pointer-events: none;
          opacity: 0;
          transition: opacity 0.16s ease;
        }
        .dambo-bar {
          position: fixed;
          top: 8px;
          right: 8px;
          display: inline-flex;
          align-items: center;
          gap: 4px;
          padding: 4px;
          border-radius: 9999px;
          background: rgba(18, 20, 26, 0.84);
          backdrop-filter: blur(12px);
          -webkit-backdrop-filter: blur(12px);
          border: 1px solid rgba(255, 255, 255, 0.18);
          box-shadow: 0 4px 14px rgba(0, 0, 0, 0.45);
          pointer-events: none;
          touch-action: none;
          opacity: 0;
          transition: opacity 0.16s ease;
        }
        .dambo-root.visible .dambo-frame {
          opacity: 1;
        }
        .dambo-root.visible .dambo-bar {
          opacity: 1;
          pointer-events: auto;
        }
        .dambo-btn {
          width: 26px;
          height: 26px;
          padding: 0;
          margin: 0;
          border: none;
          border-radius: 9999px;
          background: rgba(255, 255, 255, 0.08);
          color: rgba(245, 247, 250, 0.92);
          display: inline-flex;
          align-items: center;
          justify-content: center;
          transition: background 0.15s ease, color 0.15s ease;
        }
        .dambo-btn svg {
          pointer-events: none;
        }
        .dambo-btn:hover {
          background: rgba(255, 255, 255, 0.2);
          color: #ffffff;
        }
        .dambo-drag {
          cursor: grab;
          touch-action: none;
        }
        .dambo-drag:active {
          cursor: grabbing;
        }
        .dambo-pin {
          cursor: pointer;
          touch-action: manipulation;
        }
        .dambo-pin .dambo-pin-icon {
          transition: transform 0.16s ease, opacity 0.16s ease;
        }
        .dambo-pin:not(.pinned) {
          color: rgba(245, 247, 250, 0.52);
          background: rgba(255, 255, 255, 0.06);
        }
        .dambo-pin:not(.pinned) .dambo-pin-icon {
          transform: rotate(45deg);
        }
        .dambo-pin.pinned {
          color: #38bdf8;
          background: rgba(56, 189, 248, 0.24);
        }
        .dambo-pin.pinned:hover {
          background: rgba(56, 189, 248, 0.34);
          color: #7dd3fc;
        }
        .dambo-pin.pinned .dambo-pin-icon {
          transform: rotate(0deg);
        }
        .dambo-close {
          cursor: pointer;
          touch-action: manipulation;
        }
        .dambo-close:hover {
          background: rgba(239, 68, 68, 0.85);
          color: #ffffff;
        }
      </style>
      <div id="dambo-root" class="dambo-root">
        <div class="dambo-frame"></div>
        <div id="dambo-bar" class="dambo-bar">
          <button type="button" id="dambo-drag-btn" class="dambo-btn dambo-drag">
            <svg width="10" height="14" viewBox="0 0 10 14" fill="currentColor" aria-hidden="true">
              <circle cx="3" cy="2.5" r="1.15" />
              <circle cx="7" cy="2.5" r="1.15" />
              <circle cx="3" cy="7" r="1.15" />
              <circle cx="7" cy="7" r="1.15" />
              <circle cx="3" cy="11.5" r="1.15" />
              <circle cx="7" cy="11.5" r="1.15" />
            </svg>
          </button>
          <button type="button" id="dambo-pin-btn" class="dambo-btn dambo-pin pinned" aria-pressed="true">
            <svg class="dambo-pin-icon" width="12" height="12" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true">
              <path d="M16 4V5L14 7V12L17 15V17H13V22H11V17H7V15L10 12V7L8 5V4H16Z" />
            </svg>
          </button>
          <button type="button" id="dambo-close-btn" class="dambo-btn dambo-close">
            <svg width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.4" stroke-linecap="round" aria-hidden="true">
              <line x1="6" y1="6" x2="18" y2="18" />
              <line x1="18" y1="6" x2="6" y2="18" />
            </svg>
          </button>
        </div>
      </div>
    `;

    const rootEl = shadow.getElementById('dambo-root');
    const barEl = shadow.getElementById('dambo-bar');
    const dragBtn = shadow.getElementById('dambo-drag-btn');
    const pinBtn = shadow.getElementById('dambo-pin-btn');
    const closeBtn = shadow.getElementById('dambo-close-btn');
    dragBtn.title = DRAG_TITLE;
    dragBtn.setAttribute('aria-label', DRAG_TITLE);
    closeBtn.title = CLOSE_TITLE;
    closeBtn.setAttribute('aria-label', CLOSE_TITLE);

    let isPinned = true;
    const renderPinState = (pinned) => {
      isPinned = Boolean(pinned);
      pinBtn.classList.toggle('pinned', isPinned);
      const title = isPinned ? UNPIN_TITLE : PIN_TITLE;
      pinBtn.title = title;
      pinBtn.setAttribute('aria-label', title);
      pinBtn.setAttribute('aria-pressed', String(isPinned));
    };
    renderPinState(true);
    Promise.resolve(invokeWindowCmd('is_always_on_top'))
      .then((val) => {
        if (typeof val === 'boolean') renderPinState(val);
      })
      .catch(() => {});

    let touchHideTimer = null;
    const clearHideTimer = () => {
      if (touchHideTimer !== null) {
        clearTimeout(touchHideTimer);
        touchHideTimer = null;
      }
    };
    const showControls = (autoHideMs) => {
      rootEl.classList.add('visible');
      clearHideTimer();
      if (autoHideMs && autoHideMs > 0) {
        touchHideTimer = setTimeout(() => {
          rootEl.classList.remove('visible');
          touchHideTimer = null;
        }, autoHideMs);
      }
    };
    const hideControls = () => {
      clearHideTimer();
      rootEl.classList.remove('visible');
    };

    let activeTouchDrag = null;
    const stopTouchDrag = (keepVisible) => {
      if (!activeTouchDrag) return;
      const drag = activeTouchDrag;
      activeTouchDrag = null;
      if (drag.rafId !== null) {
        cancelAnimationFrame(drag.rafId);
      }
      window.removeEventListener('pointermove', drag.onMove);
      window.removeEventListener('pointerup', drag.onEnd);
      window.removeEventListener('pointercancel', drag.onEnd);
      dragBtn.removeEventListener('lostpointercapture', drag.onEnd);
      try {
        if (dragBtn.hasPointerCapture?.(drag.pointerId)) {
          dragBtn.releasePointerCapture(drag.pointerId);
        }
      } catch (_) {}
      if (keepVisible) {
        showControls(4000);
      }
    };

    const startTouchDrag = (startEvent) => {
      stopTouchDrag(false);
      showControls(0);

      const pointerId = startEvent.pointerId;
      const startScreenX = startEvent.screenX;
      const startScreenY = startEvent.screenY;
      let latestDx = 0;
      let latestDy = 0;
      let lastSentDx = 0;
      let lastSentDy = 0;
      let dirty = false;
      let inFlight = false;
      let rafId = null;

      try {
        dragBtn.setPointerCapture(pointerId);
      } catch (_) {}

      const initPromise = Promise.all([
        Promise.resolve(invokeWindowCmd('outer_position')),
        Promise.resolve(invokeWindowCmd('scale_factor')),
      ]).catch((err) => {
        console.error('[dambo overlay] failed to read window origin:', err);
        return null;
      });

      const pumpMove = async () => {
        if (inFlight || !dirty) return;
        inFlight = true;
        try {
          const info = await initPromise;
          if (!info || !info[0]) return;
          const origin = info[0];
          const scale = Number(info[1]) > 0 ? Number(info[1]) : (window.devicePixelRatio || 1);
          while (dirty) {
            dirty = false;
            const dx = latestDx;
            const dy = latestDy;
            if (dx === lastSentDx && dy === lastSentDy) continue;
            lastSentDx = dx;
            lastSentDy = dy;
            try {
              await invokeWindowCmd('set_position', {
                x: origin.x + dx * scale,
                y: origin.y + dy * scale,
              });
            } catch (err) {
              console.error('[dambo overlay] set_position failed:', err);
            }
          }
        } finally {
          inFlight = false;
          if (dirty) {
            pumpMove();
          }
        }
      };

      const flushMove = () => {
        rafId = null;
        if (activeTouchDrag) {
          activeTouchDrag.rafId = null;
        }
        pumpMove();
      };

      const onMove = (ev) => {
        if (ev.pointerId !== pointerId) return;
        const dx = ev.screenX - startScreenX;
        const dy = ev.screenY - startScreenY;
        if (dx === latestDx && dy === latestDy) return;
        latestDx = dx;
        latestDy = dy;
        dirty = true;
        if (rafId === null && !inFlight) {
          rafId = requestAnimationFrame(flushMove);
          if (activeTouchDrag) {
            activeTouchDrag.rafId = rafId;
          }
        }
      };

      const onEnd = (ev) => {
        if (ev && ev.pointerId !== undefined && ev.pointerId !== pointerId) return;
        if (dirty) {
          pumpMove();
        }
        stopTouchDrag(true);
      };

      activeTouchDrag = {
        pointerId,
        onMove,
        onEnd,
        rafId: null,
      };

      window.addEventListener('pointermove', onMove);
      window.addEventListener('pointerup', onEnd);
      window.addEventListener('pointercancel', onEnd);
      dragBtn.addEventListener('lostpointercapture', onEnd);
    };

    const onPointerEnter = (e) => {
      if (e.pointerType === 'mouse' || !e.pointerType) {
        showControls(0);
      }
    };
    const onPointerDownWindow = (e) => {
      if (e.pointerType && e.pointerType !== 'mouse') {
        showControls(activeTouchDrag ? 0 : 4000);
      }
    };

    document.addEventListener('pointerenter', onPointerEnter, true);
    document.addEventListener('pointermove', onPointerEnter, true);
    document.addEventListener('pointerdown', onPointerDownWindow, true);
    document.documentElement.addEventListener('pointerleave', (e) => {
      if (e.pointerType === 'mouse' || !e.pointerType) hideControls();
    });
    document.documentElement.addEventListener('mouseleave', hideControls);
    window.addEventListener('blur', () => {
      stopTouchDrag(false);
      hideControls();
    });

    barEl.addEventListener('pointerenter', () => showControls(0));
    dragBtn.addEventListener('pointerdown', (e) => {
      if (e.pointerType === 'mouse' && e.button !== 0) return;
      e.stopPropagation();
      e.preventDefault();
      if (e.pointerType === 'mouse') {
        triggerDrag();
        return;
      }
      startTouchDrag(e);
    });
    const stopBubble = (e) => {
      e.stopPropagation();
    };
    pinBtn.addEventListener('pointerdown', stopBubble);
    pinBtn.addEventListener('mousedown', stopBubble);
    pinBtn.addEventListener('click', (e) => {
      e.stopPropagation();
      e.preventDefault();
      if (e.pointerType && e.pointerType !== 'mouse') showControls(4000);
      const next = !isPinned;
      Promise.resolve(invokeWindowCmd('set_always_on_top', next))
        .then(() => {
          renderPinState(next);
        })
        .catch((err) => {
          console.error('[dambo overlay] set_always_on_top failed:', err);
        });
    });
    closeBtn.addEventListener('pointerdown', stopBubble);
    closeBtn.addEventListener('mousedown', stopBubble);
    closeBtn.addEventListener('click', (e) => {
      e.stopPropagation();
      e.preventDefault();
      triggerClose();
    });
    window.addEventListener('keydown', (e) => {
      if (e.key === 'Escape') triggerClose();
    }, true);

    (document.documentElement || document.body).appendChild(host);
  }

  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', mount, { once: true });
  } else {
    mount();
  }
})();
