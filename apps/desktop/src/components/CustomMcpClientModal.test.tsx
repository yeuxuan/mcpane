import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, expect, test, vi } from "vitest";
import "../lib/i18n";
import { CustomMcpClientModal } from "./CustomMcpClientModal";

const invokeMock = vi.fn(async (_cmd: string, _args?: unknown) => undefined);

vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string, args?: unknown) => invokeMock(cmd, args),
}));

function renderModal(onSaved = vi.fn()) {
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  render(
    <QueryClientProvider client={queryClient}>
      <CustomMcpClientModal onClose={vi.fn()} onSaved={onSaved} />
    </QueryClientProvider>,
  );
}

const nameBox = () => screen.getByPlaceholderText("Trae");
const idBox = () => screen.getByPlaceholderText("trae");
const saveButton = () => screen.getByRole("button", { name: "保存" });

beforeEach(() => {
  invokeMock.mockClear();
});

test("标识从名称派生，用户改过之后不再被名称覆盖", () => {
  renderModal();
  fireEvent.change(nameBox(), { target: { value: "Trae CN" } });
  // 派生规则要和后端的字符集校验对齐：小写 + 非字母数字转连字符。
  expect(idBox()).toHaveValue("trae-cn");

  fireEvent.change(idBox(), { target: { value: "my-trae" } });
  fireEvent.change(nameBox(), { target: { value: "Trae 国内版" } });
  // 手填过就该稳住——否则用户每敲一个字，自己填的标识都会被冲掉。
  expect(idBox()).toHaveValue("my-trae");
});

test("标识输入框拒绝非法字符", () => {
  renderModal();
  fireEvent.change(idBox(), { target: { value: "my cli!/中文" } });
  expect(idBox()).toHaveValue("mycli");
});

test("名称为空时存不下去", () => {
  renderModal();
  expect(saveButton()).toBeDisabled();
  fireEvent.change(nameBox(), { target: { value: "Trae" } });
  expect(saveButton()).toBeEnabled();
});

test("保存把规整后的标识与名称交给后端", async () => {
  const onSaved = vi.fn();
  renderModal(onSaved);
  fireEvent.change(nameBox(), { target: { value: "Trae CN" } });
  fireEvent.click(saveButton());

  await waitFor(() => expect(onSaved).toHaveBeenCalled());
  expect(invokeMock).toHaveBeenCalledWith("custom_mcp_client_upsert", {
    id: "trae-cn",
    display: "Trae CN",
  });
});

test("后端拒绝时把原因显示出来，而不是静默关掉", async () => {
  invokeMock.mockRejectedValueOnce(new Error("标识 codex 与内置客户端重名"));
  const onSaved = vi.fn();
  renderModal(onSaved);
  fireEvent.change(nameBox(), { target: { value: "Codex" } });
  fireEvent.click(saveButton());

  expect(await screen.findByText(/与内置客户端重名/)).toBeInTheDocument();
  expect(onSaved).not.toHaveBeenCalled();
});
