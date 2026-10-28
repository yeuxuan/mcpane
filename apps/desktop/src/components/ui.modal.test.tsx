import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { expect, test, vi } from "vitest";
import "../lib/i18n";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "./shadcn/dropdown-menu";
import { Popover, PopoverAnchor, PopoverContent } from "./shadcn/popover";
import { Modal } from "./ui";

/**
 * 嵌套弹窗时一次 ESC 只关最上面那层。
 *
 * ESC 监听挂在 window 上，没有栈的话两层都会收到同一次按键，一按全关——
 * 用户在外层编辑器里的位置也一并丢了。工具「注解」弹窗开在 MCP 服务器编辑
 * 弹窗里，是全仓第一处真正的嵌套，此前 Modal 没有为这种用法设计过。
 */
test("嵌套弹窗：ESC 只关最上层，下层留到下一次", () => {
  const closeOuter = vi.fn();
  const closeInner = vi.fn();
  render(
    <Modal title="外层" onClose={closeOuter}>
      <p>外层正文</p>
      <Modal title="内层" onClose={closeInner}>
        <p>内层正文</p>
      </Modal>
    </Modal>,
  );

  fireEvent.keyDown(window, { key: "Escape" });
  expect(closeInner).toHaveBeenCalledTimes(1);
  expect(closeOuter).not.toHaveBeenCalled();
});

/** 单层弹窗照旧：ESC 直接关掉，栈的引入不能改变原有行为。 */
test("单层弹窗：ESC 仍然直接关闭", () => {
  const onClose = vi.fn();
  render(
    <Modal title="只有一层" onClose={onClose}>
      <p>正文</p>
    </Modal>,
  );

  expect(screen.getByText("正文")).toBeTruthy();
  fireEvent.keyDown(window, { key: "Escape" });
  expect(onClose).toHaveBeenCalledTimes(1);
});

/** 内层关掉后，ESC 必须重新交回外层——出栈不干净的话外层就再也关不掉了。 */
test("内层卸载后 ESC 交回外层", () => {
  const closeOuter = vi.fn();
  const { rerender } = render(
    <Modal title="外层" onClose={closeOuter}>
      <Modal title="内层" onClose={vi.fn()}>
        <p>内层正文</p>
      </Modal>
    </Modal>,
  );
  rerender(
    <Modal title="外层" onClose={closeOuter}>
      <p>只剩外层</p>
    </Modal>,
  );

  fireEvent.keyDown(window, { key: "Escape" });
  expect(closeOuter).toHaveBeenCalledTimes(1);
});

/** 当前焦点的可读名：按钮/输入框取 aria-label，否则取文字。 */
function focusedName() {
  const el = document.activeElement as HTMLElement | null;
  return el?.getAttribute("aria-label") ?? el?.textContent ?? "";
}

function EditorHarness() {
  return (
    <>
      <button type="button">背景按钮</button>
      <Modal
        title="编辑"
        onClose={vi.fn()}
        footer={
          <>
            <button type="button">取消</button>
            <button type="button">保存</button>
          </>
        }
      >
        <input aria-label="名称" />
      </Modal>
    </>
  );
}

/**
 * 焦点困在弹窗内循环。真机 WKWebView 里 Option+Tab 曾一路走出弹窗、落到背景的
 * 工作区切换器上，按 Space 还会在背景里打开下拉。
 */
test("焦点循环：Tab 越过最后一个控件绕回第一个，Shift+Tab 反向绕回最后一个", async () => {
  const user = userEvent.setup();
  render(<EditorHarness />);

  const seen: string[] = [];
  for (let i = 0; i < 5; i++) {
    await user.tab();
    seen.push(focusedName());
  }
  expect(seen).toEqual(["关闭", "名称", "取消", "保存", "关闭"]);

  await user.tab({ shift: true });
  expect(focusedName()).toBe("保存");
  await user.tab({ shift: true });
  expect(focusedName()).toBe("取消");
});

/** 无人 autoFocus 时焦点在面板容器上，从这里 Shift+Tab 也得绕到最后一个，而不是跑到背景。 */
test("焦点循环：从面板容器 Shift+Tab 落到最后一个控件", async () => {
  const user = userEvent.setup();
  render(<EditorHarness />);
  expect(document.activeElement).toBe(screen.getByRole("dialog", { name: "编辑" }));

  await user.tab({ shift: true });
  expect(focusedName()).toBe("保存");
});

/** Tab 之外的途径（程序化聚焦、WKWebView 的 Option+Tab 越界）把焦点挪到背景时要被拉回。 */
test("焦点被挪到背景控件时拉回弹窗", async () => {
  const user = userEvent.setup();
  render(<EditorHarness />);
  await user.tab();
  await user.tab();
  expect(focusedName()).toBe("名称");

  screen.getByRole("button", { name: "背景按钮" }).focus();
  expect(focusedName()).toBe("名称");
});

/**
 * 真机里从行尾「…」菜单点「凭证」打开弹窗：菜单关闭后 radix 会把焦点还给背景里的触发器，
 * 此时用户还没在弹窗里动过焦点。陷阱必须照样把焦点拉回，否则 Shift+Option+Tab 一路走背景、
 * 按 Space 点中侧栏导航，页面切走、弹窗跟着没了。
 */
test("从背景菜单打开弹窗：菜单把焦点还给触发器时拉回弹窗", async () => {
  function Page() {
    const [open, setOpen] = useState(false);
    return (
      <>
        <button type="button">背景按钮</button>
        <DropdownMenu>
          <DropdownMenuTrigger asChild>
            <button type="button">更多操作</button>
          </DropdownMenuTrigger>
          <DropdownMenuContent>
            <DropdownMenuItem onSelect={() => setOpen(true)}>凭证</DropdownMenuItem>
          </DropdownMenuContent>
        </DropdownMenu>
        {open ? (
          <Modal title="凭证" onClose={() => setOpen(false)}>
            <input aria-label="名称" />
          </Modal>
        ) : null}
      </>
    );
  }
  const user = userEvent.setup();
  render(<Page />);
  await user.click(screen.getByRole("button", { name: "更多操作" }));
  await user.click(await screen.findByRole("menuitem", { name: "凭证" }));
  const dialog = await screen.findByRole("dialog", { name: "凭证" });
  // 菜单的还焦在卸载后的下一个宏任务里，等它跑完再断言。
  await waitFor(() => expect(screen.queryByRole("menu")).toBeNull());
  await new Promise((resolve) => setTimeout(resolve, 20));
  expect(dialog).toContainElement(document.activeElement as HTMLElement);

  for (let i = 0; i < 4; i++) {
    await user.tab({ shift: true });
    expect(dialog).toContainElement(document.activeElement as HTMLElement);
  }
});

test("嵌套弹窗：Tab / Shift+Tab 只在最上层循环", async () => {
  const user = userEvent.setup();
  render(
    <Modal title="外层" onClose={vi.fn()} footer={<button type="button">外层保存</button>}>
      <input aria-label="外层输入" />
      <Modal title="内层" onClose={vi.fn()} footer={<button type="button">内层确定</button>}>
        <input aria-label="内层输入" />
      </Modal>
    </Modal>,
  );
  const inner = screen.getByRole("dialog", { name: "内层" });

  for (let i = 0; i < 6; i++) {
    await user.tab();
    expect(inner).toContainElement(document.activeElement as HTMLElement);
  }
  for (let i = 0; i < 6; i++) {
    await user.tab({ shift: true });
    expect(inner).toContainElement(document.activeElement as HTMLElement);
  }
});

/** 陷阱的卸载不能把焦点拽回已经消失的弹窗；打开者仍是关窗后的落点。 */
test("关窗后焦点回到打开者", async () => {
  function Toggle() {
    const [open, setOpen] = useState(false);
    return (
      <>
        <button type="button" onClick={() => setOpen(true)}>
          打开
        </button>
        {open ? (
          <Modal title="弹窗" onClose={() => setOpen(false)}>
            <input aria-label="正文输入" />
          </Modal>
        ) : null}
      </>
    );
  }
  const user = userEvent.setup();
  render(<Toggle />);
  await user.click(screen.getByRole("button", { name: "打开" }));
  const dialog = screen.getByRole("dialog", { name: "弹窗" });
  await user.click(within(dialog).getByRole("button", { name: "关闭" }));

  expect(screen.queryByRole("dialog")).toBeNull();
  expect(focusedName()).toBe("打开");
});

/**
 * 弹窗里展开的 radix 弹层（Select / Popover）先吃掉 ESC 并 preventDefault；
 * 弹窗再跟着关就是一按全关——用户只想收起下拉。
 */
test("弹窗内的 radix 弹层先处理 ESC 时，弹窗不跟着关", async () => {
  const closeModal = vi.fn();
  const closePopover = vi.fn();
  const user = userEvent.setup();
  render(
    <Modal title="凭证" onClose={closeModal}>
      <Popover open onOpenChange={closePopover}>
        <PopoverAnchor asChild>
          <button type="button">工作区</button>
        </PopoverAnchor>
        <PopoverContent>候选列表</PopoverContent>
      </Popover>
    </Modal>,
  );

  await user.keyboard("{Escape}");
  expect(closePopover).toHaveBeenCalledWith(false);
  expect(closeModal).not.toHaveBeenCalled();
});
