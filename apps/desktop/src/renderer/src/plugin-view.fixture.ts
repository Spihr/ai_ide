/**
 * 插件声明视图的硬编码样例（docs/24 §9.1 第 3 刀）。
 *
 * 第 3 刀先在**没有真插件**的前提下把控件跑通，所以这里手写一份覆盖全部
 * 15 类控件的 `aiide.view/1` 描述。它有两个用途：
 * - 第 4 刀接线时作为「没有插件也能看见宿主」的兜底数据；
 * - 单测里当回归样本：任何一类控件被改坏，解析结果都会掉出 `ok`。
 *
 * 注意：这是**原始 payload**（未解析），故意保留几个「不完美」的地方——
 * 一条超长文本、一条含 `javascript:` 的单元格——好让限幅与降级在界面上可见。
 */

import { VIEW_SCHEMA } from './plugin-view'

/** 一份覆盖全部控件、且故意带上边界样本的视图描述。 */
export const PLUGIN_VIEW_FIXTURE = {
  schema: VIEW_SCHEMA,
  rev: 1,
  root: {
    type: 'column',
    children: [
      {
        type: 'group',
        title: '资产概览',
        children: [
          {
            type: 'row',
            children: [
              { type: 'stat', label: '顶点数', value: '128,431' },
              { type: 'stat', label: '材质槽', value: '6' },
              { type: 'stat', label: '源格式', value: 'glTF 2.0' }
            ]
          },
          { type: 'progress', value: 62, label: 'LOD 生成' },
          {
            type: 'text',
            value: '这是 asset3d 插件自己声明的视图：宿主只认控件白名单，插件代码不进渲染进程。'
          }
        ]
      },
      {
        type: 'tabs',
        items: [
          {
            key: 'meshes',
            label: '网格',
            children: [
              {
                type: 'table',
                columns: ['名称', '三角面', '材质'],
                rows: [
                  ['Body', '84,112', 'steel'],
                  ['Glass', '2,048', 'glass'],
                  ['javascript:alert(1)', '0', 'n/a']
                ]
              }
            ]
          },
          {
            key: 'nodes',
            label: '节点',
            children: [
              {
                type: 'tree',
                items: [
                  {
                    key: 'root',
                    label: 'Scene',
                    children: [
                      { key: 'body', label: 'Body' },
                      { key: 'light', label: 'KeyLight' }
                    ]
                  }
                ]
              }
            ]
          },
          {
            key: 'logs',
            label: '日志',
            children: [
              {
                type: 'list',
                items: [
                  { key: 'l1', label: '导入完成（1.2s）' },
                  { key: 'l2', label: '缺少 KHR_materials_emissive_strength，已忽略' }
                ]
              }
            ]
          }
        ]
      },
      {
        type: 'group',
        title: '渲染参数',
        children: [
          {
            type: 'row',
            children: [
              { type: 'toggle', label: '线框', props: { checked: false }, action: { command: 'asset3d.set-wireframe' } },
              { type: 'slider', label: '曝光', props: { min: -4, max: 4, step: 0.1, value: 0 }, action: { command: 'asset3d.set-exposure' } },
              {
                type: 'select',
                label: '色调映射',
                props: { value: 'aces' },
                items: [
                  { key: 'aces', label: 'ACES' },
                  { key: 'reinhard', label: 'Reinhard' }
                ],
                action: { command: 'asset3d.set-tonemap' }
              },
              { type: 'color', label: '背景', props: { value: '#14161a' }, action: { command: 'asset3d.set-background' } }
            ]
          },
          {
            type: 'row',
            children: [
              { type: 'button', label: '重新导入', action: { command: 'asset3d.reimport' } },
              // 这条命令不在样例清单里：宿主应把它渲染成禁用态（V-6）。
              { type: 'button', label: '导出 GLB', action: { command: 'asset3d.export' } }
            ]
          }
        ]
      }
    ]
  }
} as const

/** 与 fixture 配套的「本插件声明过的命令」，供按钮禁用态判定。 */
export const PLUGIN_VIEW_FIXTURE_COMMANDS = [
  'asset3d.set-wireframe',
  'asset3d.set-exposure',
  'asset3d.set-tonemap',
  'asset3d.set-background',
  'asset3d.reimport'
] as const
