/**
 * WebSocket 连接管理：连接、断线指数退避重连、消息分发。
 *
 * 在非 Tauri 环境下作为后端数据来源；Tauri 环境下优先用
 * tauri-bridge.js 的 event 监听，此模块作为兜底/浏览器调试用。
 */

import {
    ws, isIntentionalClose, reconnectAttempts, reconnectTimer,
    setWs, setReconnectAttempts, setReconnectTimer, setIsIntentionalClose,
    setAnimFrame, animFrame
} from '../state/state.js';

/** @param {'connected' | 'connecting' | 'disconnected'} state */
function updateConnectionUI(state, elStatusDot, elStatusText) {
    elStatusDot.className = 'status-indicator';
    if (state === 'connected') {
        elStatusDot.classList.add('connected');
        elStatusText.textContent = 'ACTIVE';
    } else if (state === 'connecting') {
        elStatusDot.classList.add('connecting');
        elStatusText.textContent = 'CONNECTING';
    } else {
        elStatusDot.classList.add('disconnected');
        elStatusText.textContent = 'OFFLINE';
    }
}

/**
 * @param {Object} opts
 * @param {number} opts.port
 * @param {HTMLElement} opts.elStatusDot
 * @param {HTMLElement} opts.elStatusText
 * @param {(data: any) => void} opts.onMessage - 收到 SongInfo 的回调
 * @param {() => void} opts.onConnected - 连接建立后启动渲染循环
 * @param {() => void} opts.onDisconnected
 */
export function connect({ port, elStatusDot, elStatusText, onMessage, onConnected, onDisconnected }) {
    if (ws && (ws.readyState === WebSocket.OPEN || ws.readyState === WebSocket.CONNECTING)) {
        return;
    }
    setIsIntentionalClose(false);
    updateConnectionUI('connecting', elStatusDot, elStatusText);
    if (animFrame) { cancelAnimationFrame(animFrame); setAnimFrame(0); }

    const host = location.hostname || '127.0.0.1';
    const proto = location.protocol === 'https:' ? 'wss' : 'ws';
    const wsUrl = `${proto}://${host}:${port}/ws`;

    /** @type {WebSocket} */
    let socket;
    try {
        socket = new WebSocket(wsUrl);
    } catch {
        scheduleReconnect({ port, elStatusDot, elStatusText, onMessage, onConnected, onDisconnected });
        return;
    }
    setWs(socket);

    socket.onopen = () => {
        if (ws !== socket) return;
        setReconnectAttempts(0);
        updateConnectionUI('connected', elStatusDot, elStatusText);
        onConnected();
    };

    socket.onmessage = (e) => {
        if (ws !== socket) return;
        try {
            const data = JSON.parse(e.data);
            if (data && typeof data === 'object') onMessage(data);
        } catch {}
    };

    socket.onerror = () => {
        if (ws !== socket) return;
        try { socket.close(); } catch {}
    };

    socket.onclose = () => {
        if (ws !== socket) return;
        updateConnectionUI('disconnected', elStatusDot, elStatusText);
        onDisconnected();
        if (!isIntentionalClose) {
            scheduleReconnect({ port, elStatusDot, elStatusText, onMessage, onConnected, onDisconnected });
        }
    };
}

/**
 * @param {Object} opts - 同 connect
 */
function scheduleReconnect(opts) {
    if (reconnectTimer) return;
    const delay = Math.min(15000, 1000 * Math.pow(2, reconnectAttempts));
    setReconnectAttempts(reconnectAttempts + 1);
    opts.elStatusText.textContent = `RETRY ${Math.round(delay / 1000)}S`;
    const timer = window.setTimeout(() => {
        setReconnectTimer(0);
        connect(opts);
    }, delay);
    setReconnectTimer(timer);
}

/** 主动断开，不再重连 */
export function disconnect() {
    setIsIntentionalClose(true);
    if (reconnectTimer) { clearTimeout(reconnectTimer); setReconnectTimer(0); }
    if (ws) {
        try { ws.close(); } catch {}
        setWs(null);
    }
}

export { updateConnectionUI };
