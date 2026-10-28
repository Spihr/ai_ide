/**
 * 外壳侧外部视口管辖的单元测试（docs/24 §5.4）。
 *
 * 这里钉死的都是「错了就等于开了个任意 exe 执行口子」的判定，所以每条拒绝
 * 都断言**具体原因码**而不是「抛没抛错」——原因码是面板给用户看的唯一线索，
 * 一条「启动失败」对排查毫无帮助。
 */

import { mkdtempSync, mkdirSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import type { ChildProcess } from 'node:child_process'
import { afterEach, describe, expect, it } from 'vitest'

import {
  buildViewportPipeName,
  minimalViewportEnv,
  resolveViewportProgram,
  ViewportHost,
  ViewportSpawnError,
  viewportPipePath,
  type ViewportExitInfo
} from './viewport-host'

const roots: string[] = []

function makeInstallDir(): string {
  const root = mkdtempSync(join(tmpdir(), 'aiide-viewport-'))
  roots.push(root)
  return root
}

function touch(path: string, content = ''): string {
  writeFileSync(path, content)
  return path
}

function codeOf(fn: () => unknown): string {
  try {
    fn()
  } catch (error) {
    if (error instanceof ViewportSpawnError) return error.code
    return `unexpected:${String(error)}`
  }
  return 'no-error'
}

afterEach(() => {
  for (const root of roots.splice(0)) {
    rmSync(root, { recursive: true, force: true })
  }
})

describe('resolveViewportProgram', () => {
  it('解析安装目录内的 exe', () => {
    const dir = makeInstallDir()
    const exe = touch(join(dir, 'asset3d-gui.exe'))
    expect(resolveViewportProgram(dir, 'asset3d-gui.exe')).toBe(exe)
  })

  it('允许子目录里的 exe', () => {
    const dir = makeInstallDir()
    mkdirSync(join(dir, 'bin'))
    const exe = touch(join(dir, 'bin', 'gui.exe'))
    expect(resolveViewportProgram(dir, 'bin/gui.exe')).toBe(exe)
    expect(resolveViewportProgram(dir, 'bin\\gui.exe')).toBe(exe)
  })

  it('拒绝绝对路径形式的 program', () => {
    const dir = makeInstallDir()
    expect(codeOf(() => resolveViewportProgram(dir, 'C:\\Windows\\System32\\cmd.exe'))).toBe(
      'program_invalid'
    )
    expect(codeOf(() => resolveViewportProgram(dir, '/usr/bin/sh'))).toBe('program_invalid')
  })

  it('拒绝 .. 逃逸', () => {
    const dir = makeInstallDir()
    expect(codeOf(() => resolveViewportProgram(dir, '..\\outside.exe'))).toBe('program_escape')
    expect(codeOf(() => resolveViewportProgram(dir, 'bin/../../outside.exe'))).toBe('program_escape')
  })

  it('拒绝非 exe', () => {
    const dir = makeInstallDir()
    touch(join(dir, 'run.bat'))
    expect(codeOf(() => resolveViewportProgram(dir, 'run.bat'))).toBe('program_ext')
  })

  it('拒绝包内不存在的 exe', () => {
    const dir = makeInstallDir()
    expect(codeOf(() => resolveViewportProgram(dir, 'missing.exe'))).toBe('program_missing')
  })

  it('安装目录不存在时不泄露 target 判定', () => {
    const dir = join(makeInstallDir(), 'gone')
    expect(codeOf(() => resolveViewportProgram(dir, 'a.exe'))).toBe('install_missing')
  })

  it('空 installDir / 空 program 各自报自己的原因', () => {
    const dir = makeInstallDir()
    expect(codeOf(() => resolveViewportProgram('', 'a.exe'))).toBe('install_missing')
    expect(codeOf(() => resolveViewportProgram(dir, '   '))).toBe('program_invalid')
  })

  it('拒绝含 NUL 的程序名', () => {
    const dir = makeInstallDir()
    expect(codeOf(() => resolveViewportProgram(dir, 'a\0.exe'))).toBe('program_invalid')
  })
})

describe('buildViewportPipeName', () => {
  it('带插件 id 与随机尾巴', () => {
    expect(buildViewportPipeName('asset3d', 'deadbeef')).toBe('aiide.viewport.asset3d.deadbeef')
    expect(buildViewportPipeName('hello.world')).toMatch(/^aiide\.viewport\.hello\.world\.[0-9a-f]{8}$/)
  })

  it('两次生成的管道名不同（避免与残留管道同名）', () => {
    expect(buildViewportPipeName('asset3d')).not.toBe(buildViewportPipeName('asset3d'))
  })

  it('非法插件 id 直接拒绝', () => {
    expect(codeOf(() => buildViewportPipeName('../evil'))).toBe('plugin_invalid')
    expect(codeOf(() => buildViewportPipeName(''))).toBe('plugin_invalid')
  })

  it('拼出 Windows 命名管道全名', () => {
    expect(viewportPipePath('aiide.viewport.asset3d.1')).toBe(
      '\\\\.\\pipe\\aiide.viewport.asset3d.1'
    )
  })
})

describe('minimalViewportEnv', () => {
  it('只透传最小集合，绝不带 IDE 自己注入的变量', () => {
    const env = minimalViewportEnv({
      PATH: 'C:\\Windows',
      SystemRoot: 'C:\\Windows',
      USERPROFILE: 'C:\\Users\\n',
      AI_IDE_DATA_DIR: 'C:\\secret',
      OPENAI_API_KEY: 'sk-x',
      ELECTRON_RUN_AS_NODE: '1'
    } as NodeJS.ProcessEnv)
    expect(env).toEqual({
      PATH: 'C:\\Windows',
      SystemRoot: 'C:\\Windows',
      USERPROFILE: 'C:\\Users\\n'
    })
  })

  it('空值不写进 env', () => {
    expect(minimalViewportEnv({ PATH: '' } as NodeJS.ProcessEnv)).toEqual({})
  })
})

// ---------------------------------------------------------------------------
// ViewportHost
// ---------------------------------------------------------------------------

interface FakeChild {
  exitCode: number | null
  killed: boolean
  once: (event: string, listener: (...args: unknown[]) => void) => unknown
  emit: (event: string, ...args: unknown[]) => void
}

function fakeChild(): FakeChild {
  const listeners = new Map<string, Array<(...args: unknown[]) => void>>()
  return {
    exitCode: null,
    killed: false,
    once(event, listener) {
      const list = listeners.get(event) ?? []
      list.push(listener)
      listeners.set(event, list)
      return this
    },
    emit(event, ...args) {
      for (const listener of listeners.get(event) ?? []) listener(...args)
    }
  }
}

interface HarnessOptions {
  probeBehaviour?: 'always-ok' | 'always-fail' | { failTimes: number }
  /** 只让**默认管道**连通（模拟用户手动双击起的视口）。 */
  defaultPipeAlive?: boolean
  child?: FakeChild
  onExit?: (info: ViewportExitInfo) => void
}

function harness(options: HarnessOptions = {}): {
  host: ViewportHost
  calls: Array<{ program: string; args: string[]; options: unknown }>
  child: FakeChild
  probeCount: () => number
  probePaths: () => string[]
} {
  const calls: Array<{ program: string; args: string[]; options: unknown }> = []
  const child = options.child ?? fakeChild()
  let probes = 0
  const probePaths: string[] = []
  const behaviour = options.probeBehaviour ?? 'always-ok'
  let failuresLeft = typeof behaviour === 'object' ? behaviour.failTimes : Infinity
  const host = new ViewportHost({
    attempts: 3,
    intervalMs: 0,
    onExit: options.onExit,
    sleep: () => Promise.resolve(),
    spawn: ((program: string, args: string[], spawnOptions: unknown) => {
      calls.push({ program, args, options: spawnOptions })
      return child as unknown as ChildProcess
    }) as unknown as typeof import('node:child_process').spawn,
    probe: (pipePath: string) => {
      probes += 1
      probePaths.push(pipePath)
      if (pipePath.endsWith('.default')) {
        // 默认管道代表「不是外壳起的实例」：默认没人监听，除非用例显式要求。
        return options.defaultPipeAlive === true
          ? Promise.resolve()
          : Promise.reject(new Error('ENOENT'))
      }
      if (behaviour === 'always-ok') return Promise.resolve()
      if (behaviour === 'always-fail') return Promise.reject(new Error('ENOENT'))
      if (failuresLeft > 0) {
        failuresLeft -= 1
        return Promise.reject(new Error('ENOENT'))
      }
      return Promise.resolve()
    }
  })
  return { host, calls, child, probeCount: () => probes, probePaths: () => probePaths }
}

function viewportInput(dir: string): {
  pluginId: string
  viewId: string
  installDir: string
  program: string
  protocol: string
} {
  return {
    pluginId: 'asset3d',
    viewId: 'main',
    installDir: dir,
    program: 'asset3d-gui.exe',
    protocol: 'asset3d.session/1'
  }
}

describe('ViewportHost.spawnViewport', () => {
  it('数组参数 + --pipe + 不用 shell', async () => {
    const dir = makeInstallDir()
    const exe = touch(join(dir, 'asset3d-gui.exe'))
    const { host, calls } = harness()
    const result = await host.spawnViewport(viewportInput(dir))

    expect(calls).toHaveLength(1)
    const [call] = calls
    expect(call?.program).toBe(exe)
    expect(call?.args).toEqual(['--pipe', result.pipeName])
    expect(result.reused).toBe(false)
    const spawnOptions = call?.options as { cwd: string; stdio: string }
    expect(spawnOptions.cwd).toBe(dir)
    expect(spawnOptions.stdio).toBe('ignore')
  })

  it('管道连不上时报 pipe_timeout 而不是静默成功', async () => {
    const dir = makeInstallDir()
    touch(join(dir, 'asset3d-gui.exe'))
    const { host, probeCount, probePaths } = harness({ probeBehaviour: 'always-fail' })
    await expect(host.spawnViewport(viewportInput(dir))).rejects.toMatchObject({
      code: 'pipe_timeout'
    })
    // 1 次「默认管道上有没有别人」+ attempts 次重试。
    expect(probeCount()).toBe(4)
    expect(probePaths()[0]).toContain('aiide.viewport.asset3d.default')
  })

  it('默认管道已有监听者（用户手动双击起的视口）→ 接管，不 spawn', async () => {
    const dir = makeInstallDir()
    touch(join(dir, 'asset3d-gui.exe'))
    const { host, calls } = harness({ defaultPipeAlive: true })
    const result = await host.spawnViewport(viewportInput(dir))
    expect(calls).toHaveLength(0)
    expect(result.reused).toBe(true)
    expect(result.pipeName).toBe('aiide.viewport.asset3d.default')
    // 接管时也要给出解析后的程序路径：清单合法性不该因为「正好有实例」被跳过。
    expect(result.program).toBe(join(dir, 'asset3d-gui.exe'))
  })

  it('默认管道有人但清单非法 → 仍然报错，不借道接管', async () => {
    const dir = makeInstallDir()
    const { host, calls } = harness({ defaultPipeAlive: true })
    await expect(host.spawnViewport(viewportInput(dir))).rejects.toMatchObject({
      code: 'program_missing'
    })
    expect(calls).toHaveLength(0)
  })

  it('先失败后连通（进程启动需要时间）算成功', async () => {
    const dir = makeInstallDir()
    touch(join(dir, 'asset3d-gui.exe'))
    const { host, probeCount } = harness({ probeBehaviour: { failTimes: 2 } })
    await expect(host.spawnViewport(viewportInput(dir))).resolves.toMatchObject({ reused: false })
    // 1 次默认管道探测（空）+ 2 次未就绪 + 1 次探通。
    expect(probeCount()).toBe(4)
  })

  it('进程立刻退出时报 spawn_failed（并说明退出码）', async () => {
    const dir = makeInstallDir()
    touch(join(dir, 'asset3d-gui.exe'))
    const child = fakeChild()
    child.exitCode = 2
    const { host } = harness({ child })
    await expect(host.spawnViewport(viewportInput(dir))).rejects.toMatchObject({
      code: 'spawn_failed',
      message: expect.stringContaining('退出码 2')
    })
  })

  it('协议不认识时拒绝，不 spawn', async () => {
    const dir = makeInstallDir()
    touch(join(dir, 'asset3d-gui.exe'))
    const { host, calls } = harness()
    await expect(
      host.spawnViewport({ ...viewportInput(dir), protocol: 'asset3d.session/2' })
    ).rejects.toMatchObject({ code: 'protocol_unsupported' })
    expect(calls).toHaveLength(0)
  })

  it('视图 id 不合法时拒绝（与 §4.1 同一条正则）', async () => {
    const dir = makeInstallDir()
    const { host } = harness()
    await expect(
      host.spawnViewport({ ...viewportInput(dir), viewId: 'Main View' })
    ).rejects.toMatchObject({ code: 'plugin_invalid' })
  })

  it('已有进程且管道活着 → 复用，不重复 spawn', async () => {
    const dir = makeInstallDir()
    touch(join(dir, 'asset3d-gui.exe'))
    const { host, calls } = harness()
    const first = await host.spawnViewport(viewportInput(dir))
    const second = await host.spawnViewport(viewportInput(dir))
    expect(calls).toHaveLength(1)
    expect(second.reused).toBe(true)
    expect(second.pipeName).toBe(first.pipeName)
  })

  it('进程活着但管道断了 → 重新 spawn，且换新管道名', async () => {
    const dir = makeInstallDir()
    touch(join(dir, 'asset3d-gui.exe'))
    // 第一次 spawn 后探通成功，第二次复用探测失败（模拟视口把管道关了）。
    let probes = 0
    const child = fakeChild()
    const calls: string[] = []
    const host = new ViewportHost({
      attempts: 2,
      intervalMs: 0,
      sleep: () => Promise.resolve(),
      spawn: ((program: string) => {
        calls.push(program)
        return child as unknown as ChildProcess
      }) as unknown as typeof import('node:child_process').spawn,
      probe: (pipePath: string) => {
        // 默认管道上一律没有实例，否则会走「接管」分支而离开本用例要测的复用路径。
        if (pipePath.endsWith('.default')) return Promise.reject(new Error('ENOENT'))
        probes += 1
        // 第 1 次（首次 spawn 探通）成功；第 2 次（复用检查）失败；第 3 次起成功。
        if (probes === 2) return Promise.reject(new Error('ECONNREFUSED'))
        return Promise.resolve()
      }
    })

    const first = await host.spawnViewport(viewportInput(dir))
    const second = await host.spawnViewport(viewportInput(dir))
    expect(calls).toHaveLength(2)
    expect(second.reused).toBe(false)
    expect(second.pipeName).not.toBe(first.pipeName)
  })
})

// ---------------------------------------------------------------------------
// 退出回调（docs/24 §5.3：「视口崩溃 → 面板显示视口已关闭」）
// ---------------------------------------------------------------------------

describe('ViewportHost.onExit', () => {
  it('用户关窗（退出码 0）也回调：面板要把按钮退回「打开视口」', async () => {
    const dir = makeInstallDir()
    touch(join(dir, 'asset3d-gui.exe'))
    const exits: ViewportExitInfo[] = []
    const { host, child } = harness({ onExit: (info) => exits.push(info) })

    await host.spawnViewport(viewportInput(dir))
    expect(exits).toHaveLength(0)

    child.exitCode = 0
    child.emit('exit', 0)
    expect(exits).toEqual([{ pluginId: 'asset3d', viewId: 'main', exitCode: 0 }])
  })

  it('崩溃（非 0 退出码）带出码值，崩溃与正常关闭要能分开说', async () => {
    const dir = makeInstallDir()
    touch(join(dir, 'asset3d-gui.exe'))
    const exits: ViewportExitInfo[] = []
    const { host, child } = harness({ onExit: (info) => exits.push(info) })

    await host.spawnViewport(viewportInput(dir))
    child.exitCode = 3221225477
    child.emit('exit', 3221225477)
    expect(exits).toEqual([{ pluginId: 'asset3d', viewId: 'main', exitCode: 3221225477 }])
  })

  it('error 与 exit 先后各来一次时只报一次（否则面板连着弹两条）', async () => {
    const dir = makeInstallDir()
    touch(join(dir, 'asset3d-gui.exe'))
    const exits: ViewportExitInfo[] = []
    const { host, child } = harness({ onExit: (info) => exits.push(info) })

    await host.spawnViewport(viewportInput(dir))
    child.emit('error', new Error('EPIPE'))
    child.emit('exit', 1)
    expect(exits).toHaveLength(1)
    expect(exits[0]?.exitCode).toBeNull()
  })

  it('起不来（pipe_timeout）不补发退出通知：已经报过一次失败原因了', async () => {
    const dir = makeInstallDir()
    touch(join(dir, 'asset3d-gui.exe'))
    const exits: ViewportExitInfo[] = []
    const { host, child } = harness({
      probeBehaviour: 'always-fail',
      onExit: (info) => exits.push(info)
    })

    await expect(host.spawnViewport(viewportInput(dir))).rejects.toMatchObject({
      code: 'pipe_timeout'
    })
    child.emit('exit', 1)
    expect(exits).toHaveLength(0)
  })

  it('接管别人起的实例时没有句柄，也就不会有退出回调', async () => {
    const dir = makeInstallDir()
    touch(join(dir, 'asset3d-gui.exe'))
    const exits: ViewportExitInfo[] = []
    const { host, calls } = harness({ defaultPipeAlive: true, onExit: (info) => exits.push(info) })

    await host.spawnViewport(viewportInput(dir))
    expect(calls).toHaveLength(0)
    expect(exits).toHaveLength(0)
  })
})
