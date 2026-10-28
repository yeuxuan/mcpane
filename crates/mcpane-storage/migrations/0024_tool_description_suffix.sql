-- 工具描述增强（ADR-020 主动性轴的确定性通道）：用户给下游工具追加的触发式
-- 提示，Catalog 构建时拼在原描述之后——追加不覆盖，下游改了描述提示仍在。
-- description 是平铺模式下唯一必然进模型上下文的字段，instructions 则是
-- host 可选注入；补位工具选择率只能压在这条通道上。NULL/空串 = 未配置。
ALTER TABLE workspace_tools ADD COLUMN description_suffix TEXT;
