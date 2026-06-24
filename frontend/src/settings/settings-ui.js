/**
 * 设置抽屉的 UI 交互：开关、滑块、步进器、主题选择、端口应用等。
 *
 * 把后端配置的读写与前端偏好的读写都收敛到这里，
 * index.js 只需在启动时调用 wireUp()。
 */

import { fetchBackendConfig, saveBackendConfig } from '../config/backend-config.js';
import { saveFrontendConfig } from '../config/frontend-config.js';

/**
 * @param {Object} deps
 * @param {any} els - bindElements 返回的 DOM 集合，需包含所有设置相关元素
 * @param {{front: any, back: any}} configs - 前后端配置对象的引用（可变）
 * @param {() => void} onFrontConfigChange - 前端偏好变化时回调（如重渲染歌词）
 * @param {(songInfo: any) => void} [reapplyOnSongInfo] - 偏移变化时重新应用到当前 songInfo
 */
export function wireUpSettings({ els, configs, onFrontConfigChange, reapplyOnSongInfo }) {
    const { front, back } = configs;

    // —— 抽屉开关 ——
    els.btnSettings.addEventListener('click', () => {
        fetchBackendConfig().then((cfg) => {
            Object.assign(back, cfg);
            syncConfigToUI(els, front, back);
        });
        els.settingsOverlay.classList.add('open');
    });

    els.btnSettingsClose.addEventListener('click', () => {
        els.settingsOverlay.classList.remove('open');
    });

    els.settingsOverlay.addEventListener('click', (e) => {
        if (e.target === els.settingsOverlay) els.settingsOverlay.classList.remove('open');
    });

    // —— 音频延迟补偿 ——
    els.cfgOffset.addEventListener('input', () => {
        const val = parseInt(els.cfgOffset.value, 10);
        back.offsetMs = val;
        els.valOffset.textContent = `${val}ms`;
        if (reapplyOnSongInfo) reapplyOnSongInfo();
    });
    els.cfgOffset.addEventListener('change', () => saveBackendConfig(back));

    els.btnOffsetDec.addEventListener('click', () => {
        const val = Math.max(-1000, back.offsetMs - 10);
        back.offsetMs = val;
        els.cfgOffset.value = String(val);
        els.valOffset.textContent = `${val}ms`;
        saveBackendConfig(back);
        if (reapplyOnSongInfo) reapplyOnSongInfo();
    });

    els.btnOffsetInc.addEventListener('click', () => {
        const val = Math.min(1000, back.offsetMs + 10);
        back.offsetMs = val;
        els.cfgOffset.value = String(val);
        els.valOffset.textContent = `${val}ms`;
        saveBackendConfig(back);
        if (reapplyOnSongInfo) reapplyOnSongInfo();
    });

    // —— 轮询间隔 ——
    els.cfgInterval.addEventListener('input', () => {
        els.valInterval.textContent = `${parseInt(els.cfgInterval.value, 10)}ms`;
    });
    els.cfgInterval.addEventListener('change', () => {
        back.intervalMs = parseInt(els.cfgInterval.value, 10);
        saveBackendConfig(back);
    });

    // —— 端口 ——
    els.btnApplyPort.addEventListener('click', () => {
        const val = parseInt(els.cfgPort.value, 10);
        if (!Number.isInteger(val) || val < 1 || val > 65535) {
            alert('请输入有效的端口号 (1-65535)');
            return;
        }
        back.port = val;
        saveBackendConfig(back);
        alert('同步端口已保存到 config.toml，请重启程序以启用新的服务端口。');
    });

    // —— OBS 文件输出 ——
    els.chkOutTxt.addEventListener('change', () => {
        back.outputTxt = els.chkOutTxt.checked;
        saveBackendConfig(back);
    });
    els.chkOutJson.addEventListener('change', () => {
        back.outputJson = els.chkOutJson.checked;
        saveBackendConfig(back);
    });
    els.chkOutLyric.addEventListener('change', () => {
        back.outputLyric = els.chkOutLyric.checked;
        saveBackendConfig(back);
    });

    // —— 前端外观：字号 ——
    els.cfgFontSize.addEventListener('input', () => {
        const val = parseInt(els.cfgFontSize.value, 10);
        els.valFontSize.textContent = `${val}px`;
        document.documentElement.style.setProperty('--lyric-font-size', `${val}px`);
    });
    els.cfgFontSize.addEventListener('change', () => {
        front.fontSize = parseInt(els.cfgFontSize.value, 10);
        saveFrontendConfig(front);
    });

    // —— 前端外观：字重 ——
    els.cfgFontWeight.addEventListener('input', () => {
        const val = parseInt(els.cfgFontWeight.value, 10);
        els.valFontWeight.textContent = String(val);
        document.documentElement.style.setProperty('--lyric-font-weight', `${val}`);
    });
    els.cfgFontWeight.addEventListener('change', () => {
        front.fontWeight = parseInt(els.cfgFontWeight.value, 10);
        saveFrontendConfig(front);
    });

    // —— 前端外观：翻译开关 ——
    els.toggleTrans.addEventListener('change', () => {
        front.showTranslation = els.toggleTrans.checked;
        saveFrontendConfig(front);
        onFrontConfigChange();
    });

    // —— 前端外观：调试面板 ——
    els.toggleDebug.addEventListener('change', () => {
        front.debug = els.toggleDebug.checked;
        saveFrontendConfig(front);
        els.debugPanel.hidden = !front.debug;
    });

    // —— 主题选择 ——
    document.querySelectorAll('.theme-dot').forEach((dot) => {
        dot.addEventListener('click', () => {
            const theme = dot.getAttribute('data-theme') || 'aurora-cyan';
            front.theme = theme;
            document.body.setAttribute('data-theme', theme);
            saveFrontendConfig(front);
            document.querySelectorAll('.theme-dot').forEach((d) => d.classList.remove('active'));
            dot.classList.add('active');
        });
    });
}

/**
 * 把当前配置值同步到所有 UI 控件。
 * @param {any} els
 * @param {any} front
 * @param {any} back
 */
export function syncConfigToUI(els, front, back) {
    // 后端
    els.cfgOffset.value = String(back.offsetMs);
    els.valOffset.textContent = `${back.offsetMs}ms`;
    els.cfgInterval.value = String(back.intervalMs);
    els.valInterval.textContent = `${back.intervalMs}ms`;
    els.cfgPort.value = String(back.port);
    els.chkOutTxt.checked = back.outputTxt;
    els.chkOutJson.checked = back.outputJson;
    els.chkOutLyric.checked = back.outputLyric;

    // 前端
    document.documentElement.style.setProperty('--lyric-font-size', `${front.fontSize}px`);
    document.documentElement.style.setProperty('--lyric-font-weight', `${front.fontWeight}`);
    document.body.setAttribute('data-theme', front.theme);
    els.cfgFontSize.value = String(front.fontSize);
    els.valFontSize.textContent = `${front.fontSize}px`;
    els.cfgFontWeight.value = String(front.fontWeight);
    els.valFontWeight.textContent = String(front.fontWeight);
    els.toggleTrans.checked = front.showTranslation;
    els.toggleDebug.checked = front.debug;
    els.debugPanel.hidden = !front.debug;

    // 主题点
    document.querySelectorAll('.theme-dot').forEach((dot) => {
        const theme = dot.getAttribute('data-theme');
        if (theme === front.theme) dot.classList.add('active');
        else dot.classList.remove('active');
    });
}
