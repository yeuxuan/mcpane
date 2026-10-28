# 品牌图标资源

这里只放**官方 logo**，来源必须可追溯。禁止手绘"意思到了"的近似图形——
形状不对的图标比没有图标更糟，用户会以为认错了客户端。

矢量 logo 直接内联进 `components/BrandIcon.tsx`；只有官方没发矢量版时才落文件到这里。

| 文件 | 来源 | 加工 |
| --- | --- | --- |
| `hermes.png` | `https://hermes-agent.nousresearch.com/favicon.ico`（NousResearch/hermes-agent） | 官方只有位图，且是**白底黑线**。按亮度转成 alpha 蒙版（`alpha = 255 - 亮度`，颜色置黑），这样能当 CSS mask 用 currentColor 上色，深浅色主题都跟 IconChip 的色调一致；直接贴原图会在图标位糊出一块白方片。 |

新增客户端时，先去官网 `/favicon.svg`、GitHub 仓库的 `assets/`、simple-icons、lobe-icons 找官方矢量图，
都没有再考虑位图走上面的蒙版流程。

第三方品牌标识仅用于产品内的识别用途（nominative use），归各自权利人所有；
收录不代表该客户端或供应商与 MCPane 有任何关联或背书。
