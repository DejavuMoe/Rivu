# 世界地图形状的来源

`countries-110m.ts` 是看板世界地图（`../WorldMap.svelte`）用的国家形状，由 `designs/rivu-svelte/scripts/gen-world-map.mjs` 生成，不要手改。

| 项 | 内容 |
|---|---|
| 数据 | Natural Earth 1:110m Cultural Vectors，Admin 0 – Countries（`ne_110m_admin_0_countries`），版本 5.1.2 |
| 下载地址 | <https://raw.githubusercontent.com/nvkelso/natural-earth-vector/v5.1.2/geojson/ne_110m_admin_0_countries.geojson> |
| 输入文件 sha256 | `6866c877d39cba9c357620878839b336d569f8c662d3cfab4cb1dbe2d39c977f` |
| 许可 | 公有领域（public domain）。Natural Earth 的使用条款：<https://www.naturalearthdata.com/about/terms-of-use/>，不要求署名 |
| 下载日期 | 2026-11-20 |

## 处理

1. 去掉南极洲。
2. 国家按 ISO 3166-1 两字母代码：先取 `ISO_A2_EH`，再取 `ISO_A2`（法国、挪威的 `ISO_A2` 是 `-99`，台湾是 `CN-TW`，取 `ISO_A2_EH` 后分别是 `FR`、`NO`、`TW`；科索沃是 `XK`，与地理库一致）。两块没有代码的区域并入地理库给出的国家：N. Cyprus → `CY`，Somaliland → `SO`。
3. Equal Earth 投影，放到宽 1000、高 442 的整数网格上（纬度范围南纬 60 度到北纬 84 度），去掉重复点、共线点和投影后面积小于 1 个单位的小岛。
4. 每个国家写成一条相对坐标的 SVG 路径。

形状的边界只是示意，不代表对任何领土归属的立场。

## 更新

下载新版本的 GeoJSON（放在仓库之外的目录），在 `designs/rivu-svelte` 里运行：

```bash
node scripts/gen-world-map.mjs <ne_110m_admin_0_countries.geojson>
```

然后更新本文件的版本、地址、sha256 和日期，并把生成的文件与本文件一起复制到 `apps/dashboard/src/lib/charts/world/`（两边必须逐字节相同）。
