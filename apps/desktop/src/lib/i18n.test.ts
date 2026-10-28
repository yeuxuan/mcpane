import { expect, it } from "vitest";
import i18n from "./i18n";

/**
 * 中英两份文案的键必须一一对应。
 *
 * 缺键不会报错——i18next 会静默回落到中文，界面上只是某一行忽然变成中文。
 * e2e 里那条「英文后各页正文不再残留中文」能抓到一部分，但它只走遍历得到的
 * 页面：弹层、导览这些要靠交互才出现的地方扫不到（导览在 e2e fixture 里干脆
 * 被预置成已完成）。所以这一层从数据本身比对，不依赖界面走到没走到。
 */
it("中英文案的键一一对应", () => {
  const zh = Object.keys(i18n.getResourceBundle("zh-CN", "translation"));
  const en = Object.keys(i18n.getResourceBundle("en", "translation"));
  const enSet = new Set(en);
  const zhSet = new Set(zh);
  expect({
    缺英文: zh.filter((k) => !enSet.has(k)),
    缺中文: en.filter((k) => !zhSet.has(k)),
  }).toEqual({ 缺英文: [], 缺中文: [] });
});

/**
 * 代码里引用的每个静态 key 都必须真的存在。
 *
 * 上面那条只比对两份资源之间的对齐，管不到"代码引用了一个谁也没定义的 key"：
 * i18next 对缺失 key 的回落是**原样显示 key 本身**，于是按钮上会赫然写着
 * `common.save`。这类错在类型层面无迹可寻，评审也容易滑过去——只有真跑到那个
 * 弹层才看得见，而弹层恰恰是 e2e 最难覆盖的地方。
 */
it("代码引用的静态 key 都已定义", () => {
  const sources = import.meta.glob("../**/*.{ts,tsx}", {
    query: "?raw",
    import: "default",
    eager: true,
  }) as Record<string, string>;
  const defined = new Set(Object.keys(i18n.getResourceBundle("zh-CN", "translation")));
  // 前置断言排掉 expect( / format( 这类同样以 t 结尾的调用。
  const callRe = /(?<![\w.])t\(\s*"([a-zA-Z][\w.]*)"/g;

  const missing = new Set<string>();
  for (const [path, code] of Object.entries(sources)) {
    if (path.includes(".test.")) continue;
    for (const [, key] of code.matchAll(callRe)) {
      if (key && !defined.has(key)) missing.add(`${key}（${path}）`);
    }
  }
  expect([...missing]).toEqual([]);
});
