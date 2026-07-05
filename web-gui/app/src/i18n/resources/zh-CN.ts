import type { EnResource } from "./en";

/**
 * Simplified Chinese translation resources.
 * Must keep keys structurally identical to {@link EnResource}.
 */
const zhCN: EnResource = {
  settings: {
    language: {
      label: "语言",
      description: "界面显示语言",
      systemResolved: "系统语言：{{language}}",
      system: "跟随系统",
      english: "English",
      chineseSimplified: "简体中文",
    },
    general: {
      label: "通用",
      description: "连接与运行时基础设置",
    },
    models: { label: "模型", description: "默认模型与 Provider 密钥" },
    vision: { label: "视觉", description: "图像观察模型" },
    search: { label: "搜索", description: "路由与搜索 Provider" },
    advanced: { label: "高级", description: "诊断与原始配置" },
  },
  common: {
    refreshing: "刷新中…",
    refresh: "刷新",
  },
};

export default zhCN;
