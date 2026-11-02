/**
 * 首次启动 3 步引导（UR-02，FR 新手画像）：
 * 新建笔记 → 双链 → 检索。
 * - 标记保存在 localStorage('monote.onboardingDone')，同时尽力写 config.onboardingDone；
 * - 任意步骤可跳过；Esc 跳过；焦点限制在引导卡片内。
 */
import { ctx } from './ctx.js';
import { state } from './state.js';
import { el, clear, focusTrap } from './dom.js';
import { toast } from './toast.js';

const STORAGE_KEY = 'monote.onboardingDone';
const TOTAL_STEPS = 3;

let overlayEl = null;
let trap = null;
let step = 0;

const STEPS = [
  {
    icon: '📝',
    title: '第 1 步 · 新建一篇笔记',
    body: '点击顶部「＋ 新建」（或按 Ctrl+N）创建笔记。笔记就是磁盘上的标准 .md 文件，自动保存到你的笔记库，无需担心丢失。',
    demo: '新建后，左侧文件树会出现这篇笔记；\n标题与标签可在「属性」面板中修改。',
    action: { label: '现在新建一篇', run: () => ctx.actions.createNote({}) },
  },
  {
    icon: '🔗',
    title: '第 2 步 · 用双链连接知识',
    body: '在编辑器中输入两个左方括号 [[ 会弹出补全列表，选择一篇笔记即可建立双向链接；也可以手写 [[笔记名]]。预览中点击双链即可跳转。',
    demo: '示例：[[欢迎使用 MoNote]]\n如果目标不存在，预览会以红色缺失样式显示，点击即可创建。',
    action: null,
  },
  {
    icon: '🔍',
    title: '第 3 步 · 快速检索',
    body: '按 Ctrl+P 按标题快速打开笔记；按 Ctrl+Shift+F 在全部笔记的标题与正文中全文检索，支持 tag:标签 与 "精确短语"。',
    demo: '试试：Ctrl+P 输入「操作」；\n或 Ctrl+Shift+F 输入 内存 tag:考研',
    action: { label: '打开全文检索', run: () => { finish(); ctx.search.openFull({}); } },
  },
];

export function init() {
  return { maybeShow, open, skip: finish };
}

/** 若未完成引导则展示。 */
export function maybeShow() {
  if (isDone()) return;
  // 未打开库时先不打扰（欢迎页优先）
  if (!state.vault) return;
  open();
}

function isDone() {
  try {
    if (localStorage.getItem(STORAGE_KEY) === '1') return true;
  } catch {
    /* ignore */
  }
  return !!(state.config && state.config.onboardingDone);
}

function markDone() {
  try {
    localStorage.setItem(STORAGE_KEY, '1');
  } catch {
    /* ignore */
  }
  if (state.config && !state.config.onboardingDone) {
    state.config.onboardingDone = true;
    ctx.api.callSafe('app_set_config', { patch: { onboardingDone: true } }, null).catch(() => {});
  }
}

/** 打开引导（可从设置/命令面板再次查看）。 */
export function open() {
  close();
  step = 0;
  const root = document.getElementById('onboarding-root') || document.body;
  overlayEl = el('div', { className: 'onboarding-overlay', role: 'presentation' });
  root.appendChild(overlayEl);
  render();
  requestAnimationFrame(() => {
    const card = overlayEl ? overlayEl.querySelector('.onboarding-card') : null;
    if (card) trap = focusTrap(card, { initialFocus: card.querySelector('[data-onboarding-next]') || card });
  });
}

function close() {
  if (overlayEl) {
    overlayEl.remove();
    overlayEl = null;
  }
  if (trap) {
    trap.release();
    trap = null;
  }
}

function finish() {
  markDone();
  close();
  toast.success('引导已完成；随时可在「命令面板」中重新查看');
}

function goNext() {
  if (step >= TOTAL_STEPS - 1) {
    finish();
    return;
  }
  step += 1;
  render();
  requestAnimationFrame(() => {
    const card = overlayEl ? overlayEl.querySelector('.onboarding-card') : null;
    if (card) {
      const btn = card.querySelector('[data-onboarding-next]');
      if (btn) btn.focus();
    }
  });
}

function goPrev() {
  if (step <= 0) return;
  step -= 1;
  render();
}

function render() {
  if (!overlayEl) return;
  clear(overlayEl);
  const current = STEPS[step];
  const dots = el('div', { className: 'onboarding-progress', 'aria-hidden': 'true' }, STEPS.map((_, index) => el('span', {
    className: `onboarding-dot${index <= step ? ' active' : ''}`,
  })));

  const skipBtn = el('button', { type: 'button', className: 'btn btn-sm btn-ghost', text: '跳过引导' });
  skipBtn.addEventListener('click', finish);

  const prevBtn = el('button', { type: 'button', className: 'btn btn-sm', text: '上一步' });
  prevBtn.disabled = step === 0;
  prevBtn.addEventListener('click', goPrev);

  const nextBtn = el('button', {
    type: 'button',
    className: 'btn btn-primary',
    text: step >= TOTAL_STEPS - 1 ? '开始使用' : '下一步',
    'data-onboarding-next': '1',
  });
  nextBtn.addEventListener('click', goNext);

  const actions = el('div', { className: 'onboarding-actions' }, [
    skipBtn,
    el('div', { className: 'right' }, [
      current.action
        ? (() => {
            const actionBtn = el('button', { type: 'button', className: 'btn', text: current.action.label });
            actionBtn.addEventListener('click', () => {
              close();
              Promise.resolve(current.action.run()).catch((err) => toast.error(ctx.api.friendlyError(err)));
            });
            return actionBtn;
          })()
        : null,
      prevBtn,
      nextBtn,
    ].filter(Boolean)),
  ]);

  const card = el('div', {
    className: 'onboarding-card',
    role: 'dialog',
    'aria-modal': 'true',
    'aria-labelledby': 'onboarding-title',
    tabindex: '-1',
  }, [
    dots,
    el('div', { className: 'onboarding-icon', 'aria-hidden': 'true', text: current.icon }),
    el('h2', { id: 'onboarding-title', text: current.title }),
    el('p', { text: current.body }),
    el('div', { className: 'onboarding-demo', text: current.demo }),
    actions,
    el('p', { className: 'hint', text: `第 ${step + 1} / ${TOTAL_STEPS} 步 · 引导只显示一次，可用命令面板「重新查看引导」再次打开` }),
  ]);
  overlayEl.appendChild(card);
  card.addEventListener('keydown', (event) => {
    if (event.key === 'Escape') {
      event.preventDefault();
      finish();
    } else if (event.key === 'ArrowRight' && event.target === overlayEl) {
      goNext();
    }
  });
}

export default { init, maybeShow, open };
