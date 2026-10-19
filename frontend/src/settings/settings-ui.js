/**
 * 设置抽屉的 UI 交互：开关、滑块、步进器、主题选择、端口应用等。
 *
 * 把后端配置的读写与前端偏好的读写都收敛到这里，
 * index.js 只需在启动时调用 wireUp()。
 */

import { fetchBackendConfig, saveBackendConfig } from '../config/backend-config.js';
import { saveFrontendConfig } from '../config/frontend-config.js';
import { paintCustomTheme, presetThemeColor, loadAutoPalette } from '../config/theme.js';

/**
 * Tauri 环境：向后端同步状态栏歌词窗口开关。
 * side / gap 必须随开关一起下发：后端在创建歌词条窗口的同一时刻就要用
 * 这两个值做首次定位，单独追加的 set_lyric_bar_side 调用会晚于首次定位，
 * 表现为「第一次打开时位置与设置不符」。
 */
function syncLyricBarToBackend(enabled, front) {
    if (window.__TAURI__) {
        window.__TAURI__.core.invoke('set_lyric_bar_enabled', {
            enabled,
            side: front.lyricBarSide,
            gap: front.lyricBarGap || 0
        }).catch((e) => {
            console.error('Failed to toggle lyric bar:', e);
        });
    }
}

/** 主题变化时广播给其他窗口（状态栏歌词条等）实时同步 */
function notifyThemeChanged() {
    if (window.__TAURI__) {
        window.__TAURI__.event.emit('theme-changed').catch(() => {});
    }
}

/** 设置歌词条位置偏好并通知后端重新定位 */
function setLyricBarSide(els, front, side) {
    front.lyricBarSide = side;
    saveFrontendConfig(front);
    markLyricSide(els, front);
    if (window.__TAURI__ && front.lyricBar) {
        window.__TAURI__.core.invoke('set_lyric_bar_side', { side }).catch((e) => {
            console.error('Failed to set lyric bar side:', e);
        });
    }
}

/** 高亮当前歌词条位置按钮 */
function markLyricSide(els, front) {
    els.btnLyricSideLeft.classList.toggle('active', front.lyricBarSide === 'left');
    els.btnLyricSideRight.classList.toggle('active', front.lyricBarSide === 'right');
}

/** 高亮当前任务栏配色按钮 */
function markTaskbarTheme(els, front) {
    els.btnTaskbarThemeAuto.classList.toggle('active', front.taskbarTheme === 'auto');
    els.btnTaskbarThemeDark.classList.toggle('active', front.taskbarTheme === 'dark');
    els.btnTaskbarThemeLight.classList.toggle('active', front.taskbarTheme === 'light');
}

/** 设置任务栏配色偏好（自动/深色/浅色） */
function setTaskbarTheme(els, front, theme, changed) {
    front.taskbarTheme = theme;
    saveFrontendConfig(front);
    markTaskbarTheme(els, front);
    if (typeof changed === 'function') changed();
}

/**
 * @param {Object} deps
 * @param {any} els - bindElements 返回的 DOM 集合，需包含所有设置相关元素
 * @param {{front: any, back: any}} configs - 前后端配置对象的引用（可变）
 * @param {() => void} onFrontConfigChange - 前端偏好变化时回调（如重渲染歌词）
 */
export function wireUpSettings({ els, configs, onFrontConfigChange }) {
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
    });
    els.cfgOffset.addEventListener('change', () => saveBackendConfig(back));

    els.btnOffsetDec.addEventListener('click', () => {
        const val = Math.max(-1000, back.offsetMs - 10);
        back.offsetMs = val;
        els.cfgOffset.value = String(val);
        els.valOffset.textContent = `${val}ms`;
        saveBackendConfig(back);
    });

    els.btnOffsetInc.addEventListener('click', () => {
        const val = Math.min(1000, back.offsetMs + 10);
        back.offsetMs = val;
        els.cfgOffset.value = String(val);
        els.valOffset.textContent = `${val}ms`;
        saveBackendConfig(back);
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

    // —— 状态栏歌词 ——
    els.toggleLyricBar.addEventListener('change', () => {
        front.lyricBar = els.toggleLyricBar.checked;
        saveFrontendConfig(front);
        syncLyricBarToBackend(front.lyricBar, front);
    });

    // —— 歌词条位置（左/右） ——
    els.btnLyricSideLeft.addEventListener('click', () => setLyricBarSide(els, front, 'left'));
    els.btnLyricSideRight.addEventListener('click', () => setLyricBarSide(els, front, 'right'));

    // —— 任务栏配色（自动/深色/浅色） ——
    els.btnTaskbarThemeAuto.addEventListener('click', () => setTaskbarTheme(els, front, 'auto', onFrontConfigChange));
    els.btnTaskbarThemeDark.addEventListener('click', () => setTaskbarTheme(els, front, 'dark', onFrontConfigChange));
    els.btnTaskbarThemeLight.addEventListener('click', () => setTaskbarTheme(els, front, 'light', onFrontConfigChange));

    // —— 歌词条边距（距任务栏边缘；左侧=距左缘，右侧=距通知区左缘） ——
    els.cfgLyricBarGap.addEventListener('input', () => {
        els.valLyricBarGap.textContent = `${parseInt(els.cfgLyricBarGap.value, 10)}px`;
    });
    els.cfgLyricBarGap.addEventListener('change', () => {
        front.lyricBarGap = parseInt(els.cfgLyricBarGap.value, 10);
        saveFrontendConfig(front);
        if (window.__TAURI__ && front.lyricBar) {
            window.__TAURI__.core.invoke('set_lyric_bar_gap', { gap: front.lyricBarGap }).catch((e) => {
                console.error('Failed to set lyric bar gap:', e);
            });
        }
    });

    // —— 主题选择 ——
    document.querySelectorAll('.theme-dot[data-theme]').forEach((dot) => {
        dot.addEventListener('click', () => {
            const theme = dot.getAttribute('data-theme') || 'aurora-cyan';
            applyTheme(els, front, theme);
        });
    });

    // —— 自定义取色盘 ——
    // 取色按钮驱动 0 尺寸隐藏输入打开原生取色器
    els.themeDotCustom.addEventListener('click', () => {
        els.customColor.click();
    });
    els.customColor.addEventListener('input', () => {
        const hex = els.customColor.value;
        front.theme = 'custom';
        front.customColor = hex;
        document.body.setAttribute('data-theme', 'custom');
        paintCustomTheme(hex);
        saveFrontendConfig(front);
        markActiveTheme('custom');
        notifyThemeChanged();
    });
}

/** 应用主题：预设主题名 / 'auto'（专辑取色）/ 'custom'（配合 customColor 使用） */
function applyTheme(els, front, theme) {
    front.theme = theme;
    document.body.setAttribute('data-theme', theme);
    if (theme === 'auto') {
        // 自动主题：应用最近一次专辑调色板（含 uiMode，浅色封面会切浅色 UI）
        paintCustomTheme(loadAutoPalette());
        els.customColor.value = presetThemeColor(theme);
    } else {
        paintCustomTheme(null); // 清除变量覆盖，回落到 token 定义
        if (theme === 'custom') {
            paintCustomTheme(front.customColor || els.customColor.value);
        } else {
            // 取色盘随主题显示一个参考色，方便接着微调
            els.customColor.value = presetThemeColor(theme);
        }
    }
    saveFrontendConfig(front);
    markActiveTheme(theme);
    notifyThemeChanged();
}

/** 高亮当前激活的主题圆点 */
function markActiveTheme(theme) {
    document.querySelectorAll('.theme-dot').forEach((d) => {
        d.classList.toggle('active', theme === 'custom'
            ? d.id === 'theme-dot-custom'
            : d.getAttribute('data-theme') === theme);
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

    // 前端
    document.documentElement.style.setProperty('--lyric-font-size', `${front.fontSize}px`);
    document.documentElement.style.setProperty('--lyric-font-weight', `${front.fontWeight}`);
    document.body.setAttribute('data-theme', front.theme);
    if (front.theme === 'auto') {
        paintCustomTheme(loadAutoPalette());
        els.customColor.value = presetThemeColor(front.theme);
    } else if (front.theme === 'custom') {
        paintCustomTheme(front.customColor || els.customColor.value);
        els.customColor.value = front.customColor || els.customColor.value;
    } else {
        paintCustomTheme(null);
        els.customColor.value = presetThemeColor(front.theme);
    }
    els.cfgFontSize.value = String(front.fontSize);
    els.valFontSize.textContent = `${front.fontSize}px`;
    els.cfgFontWeight.value = String(front.fontWeight);
    els.valFontWeight.textContent = String(front.fontWeight);
    els.toggleTrans.checked = front.showTranslation;
    els.toggleLyricBar.checked = front.lyricBar;
    els.toggleDebug.checked = front.debug;
    els.cfgLyricBarGap.value = String(front.lyricBarGap || 0);
    els.valLyricBarGap.textContent = `${front.lyricBarGap || 0}px`;
    els.debugPanel.hidden = !front.debug;
    markLyricSide(els, front);
    markTaskbarTheme(els, front);

    // 主题点
    markActiveTheme(front.theme);
}
