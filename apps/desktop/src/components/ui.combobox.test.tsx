import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { expect, test } from "vitest";
import "../lib/i18n";
import { ModelCombobox } from "./ui";

function Harness({ options, initial = "" }: { options: string[]; initial?: string }) {
  const [value, setValue] = useState(initial);
  return (
    <>
      <ModelCombobox aria-label="模型" value={value} onChange={setValue} options={options} />
      <output data-testid="value">{value}</output>
    </>
  );
}

const MODELS = ["gpt-5.6-sol", "gpt-5.6-luna", "deepseek-v4-flash", "glm-5.3"];

test("输入即过滤候选（大小写不敏感），Enter 采用高亮项", async () => {
  const user = userEvent.setup();
  render(<Harness options={MODELS} />);
  const input = screen.getByRole("combobox", { name: "模型" });

  await user.type(input, "GPT");
  const options = await screen.findAllByRole("option");
  expect(options.map((o) => o.textContent)).toEqual(["gpt-5.6-sol", "gpt-5.6-luna"]);

  await user.keyboard("{ArrowDown}{Enter}");
  expect(screen.getByTestId("value")).toHaveTextContent("gpt-5.6-luna");
  expect(screen.queryByRole("listbox")).toBeNull();
});

test("不在候选里的模型名照样保留——候选只是加速，不是限制", async () => {
  const user = userEvent.setup();
  render(<Harness options={MODELS} />);
  await user.type(screen.getByRole("combobox", { name: "模型" }), "my-custom-model");
  expect(screen.queryByRole("listbox")).toBeNull();
  expect(screen.getByTestId("value")).toHaveTextContent("my-custom-model");
});

test("没有候选时就是普通输入框，不弹层", async () => {
  const user = userEvent.setup();
  render(<Harness options={[]} />);
  const input = screen.getByRole("combobox", { name: "模型" });
  await user.click(input);
  await user.keyboard("{ArrowDown}");
  expect(screen.queryByRole("listbox")).toBeNull();
  expect(input).toHaveAttribute("aria-expanded", "false");
});

test("点候选采用该项（mousedown 抢在 blur 之前）", async () => {
  const user = userEvent.setup();
  render(<Harness options={MODELS} />);
  await user.type(screen.getByRole("combobox", { name: "模型" }), "glm");
  await user.click(await screen.findByRole("option", { name: "glm-5.3" }));
  expect(screen.getByTestId("value")).toHaveTextContent("glm-5.3");
});
