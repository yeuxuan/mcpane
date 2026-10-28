import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import "../lib/i18n";
import { JsonEditorView } from "./JsonEditorView";

describe("JsonEditorView", () => {
  it("挂载与卸载都不抛错，host 容器渲染出来", () => {
    const onChange = vi.fn();
    const { container, unmount } = render(<JsonEditorView value='{"a":1}' onChange={onChange} />);
    expect(container.querySelector(".cm-editor")).not.toBeNull();
    expect(() => unmount()).not.toThrow();
  });

  it("非法 JSON 时格式化按钮禁用", () => {
    render(<JsonEditorView value="{not json" onChange={vi.fn()} />);
    const button = screen.getByRole("button", { name: /格式化|format/i });
    expect(button).toBeDisabled();
  });

  it("合法 JSON 时格式化按钮可点，点击后回调美化后的文本", async () => {
    const user = userEvent.setup();
    const onChange = vi.fn();
    render(<JsonEditorView value='{"a":1,"b":2}' onChange={onChange} />);
    const button = screen.getByRole("button", { name: /格式化|format/i });
    expect(button).toBeEnabled();
    await user.click(button);
    expect(onChange).toHaveBeenCalledWith(JSON.stringify({ a: 1, b: 2 }, null, 2));
  });

  it("TOML 模式下格式化走 smol-toml stringify", async () => {
    const user = userEvent.setup();
    const onChange = vi.fn();
    render(<JsonEditorView value="a = 1" onChange={onChange} language="toml" />);
    const button = screen.getByRole("button", { name: /格式化|format/i });
    await waitFor(() => expect(button).toBeEnabled());
    await user.click(button);
    expect(onChange).toHaveBeenCalledWith("a = 1\n");
  });

  it("disabled 时格式化按钮禁用、host 容器带 pointer-events-none", () => {
    const { container } = render(<JsonEditorView value='{"a":1}' onChange={vi.fn()} disabled />);
    expect(screen.getByRole("button", { name: /格式化|format/i })).toBeDisabled();
    expect(container.querySelector(".pointer-events-none")).not.toBeNull();
  });

  it("hasError 时 host 容器带描红边框类", () => {
    const { container } = render(<JsonEditorView value='{"a":1}' onChange={vi.fn()} hasError />);
    expect(container.querySelector(".border-danger")).not.toBeNull();
  });
});
