import type { ScreenSnapshot } from "./types";
import {
  ATLAS_CELL_HEIGHT,
  ATLAS_CELL_WIDTH,
  CELL_HEIGHT,
  CELL_WIDTH,
  terminalViewport,
} from "./terminal-geometry";

export type PaneRenderModel = {
  id: string;
  snapshot?: ScreenSnapshot;
  x: number;
  y: number;
  width: number;
  height: number;
  active: boolean;
};

const FLOATS_PER_VERTEX = 9;
const FONT = '600 32px "SFMono-Semibold", "SF Mono", Menlo, Monaco, monospace';
const ATLAS_COLUMNS = 32;
const MAX_ATLAS_GLYPHS = 4096;

export async function requestWebGpuDevice(): Promise<GPUDevice> {
  if (!navigator.gpu) throw new Error("WebGPUを利用できません");
  const adapter = await navigator.gpu.requestAdapter({
    powerPreference: "high-performance",
  });
  if (!adapter) throw new Error("WebGPU adapterを取得できません");
  return adapter.requestDevice();
}

export class TerminalRenderer {
  #context: GPUCanvasContext;
  #format: GPUTextureFormat;
  #pipeline: GPURenderPipeline;
  #sampler: GPUSampler;
  #bindGroupLayout: GPUBindGroupLayout;
  #device: GPUDevice;
  #canvas: HTMLCanvasElement;
  #lost = false;
  #renderCount = 0;
  onDeviceLost: (reason: string) => void = () => undefined;

  constructor(canvas: HTMLCanvasElement, device: GPUDevice) {
    this.#canvas = canvas;
    this.#device = device;
    const context = canvas.getContext("webgpu");
    if (!context) throw new Error("WebGPU canvas contextを取得できません");
    this.#context = context;
    this.#format = navigator.gpu.getPreferredCanvasFormat();
    this.#context.configure({
      device,
      format: this.#format,
      alphaMode: "opaque",
    });
    this.#bindGroupLayout = device.createBindGroupLayout({
      entries: [
        { binding: 0, visibility: GPUShaderStage.FRAGMENT, sampler: {} },
        { binding: 1, visibility: GPUShaderStage.FRAGMENT, texture: {} },
      ],
    });
    device.pushErrorScope("validation");
    const shader = device.createShaderModule({ code: SHADER });
    void shader.getCompilationInfo().then((info) => {
      const errors = info.messages
        .filter((message) => message.type === "error")
        .map(
          (message) =>
            `${message.lineNum}:${message.linePos} ${message.message}`,
        )
        .join("\n");
      if (errors) this.#canvas.dataset.shaderError = errors;
      else delete this.#canvas.dataset.shaderError;
    });
    this.#pipeline = device.createRenderPipeline({
      layout: device.createPipelineLayout({
        bindGroupLayouts: [this.#bindGroupLayout],
      }),
      vertex: {
        module: shader,
        entryPoint: "vs_main",
        buffers: [
          {
            arrayStride: FLOATS_PER_VERTEX * 4,
            attributes: [
              { shaderLocation: 0, offset: 0, format: "float32x2" },
              { shaderLocation: 1, offset: 8, format: "float32x2" },
              { shaderLocation: 2, offset: 16, format: "float32x4" },
              { shaderLocation: 3, offset: 32, format: "float32" },
            ],
          },
        ],
      },
      fragment: {
        module: shader,
        entryPoint: "fs_main",
        targets: [
          {
            format: this.#format,
            blend: {
              color: {
                srcFactor: "src-alpha",
                dstFactor: "one-minus-src-alpha",
              },
              alpha: { srcFactor: "one", dstFactor: "one-minus-src-alpha" },
            },
          },
        ],
      },
      primitive: { topology: "triangle-list" },
    });
    this.#sampler = device.createSampler({
      magFilter: "linear",
      minFilter: "linear",
    });
    void device.popErrorScope().then((gpuError) => {
      if (gpuError) this.#canvas.dataset.pipelineError = gpuError.message;
      else delete this.#canvas.dataset.pipelineError;
    });
    void device.lost.then((info) => {
      this.#lost = true;
      this.onDeviceLost(info.message || info.reason);
    });
  }

  render(panes: PaneRenderModel[]): void {
    if (this.#lost) return;
    this.#device.pushErrorScope("validation");
    this.resize();
    const atlas = buildAtlas(panes);
    const texture = this.#device.createTexture({
      size: [atlas.canvas.width, atlas.canvas.height],
      format: "rgba8unorm",
      usage:
        GPUTextureUsage.TEXTURE_BINDING |
        GPUTextureUsage.COPY_DST |
        GPUTextureUsage.RENDER_ATTACHMENT,
    });
    this.#device.queue.copyExternalImageToTexture(
      { source: atlas.canvas },
      { texture },
      [atlas.canvas.width, atlas.canvas.height],
    );
    const vertices = buildVertices(
      panes,
      atlas.glyphs,
      atlas.canvas.height / ATLAS_CELL_HEIGHT,
      this.#canvas.clientWidth,
      this.#canvas.clientHeight,
    );
    this.#canvas.dataset.renderCount = String(++this.#renderCount);
    this.#canvas.dataset.renderedPanes = String(panes.length);
    this.#canvas.dataset.vertexCount = String(
      vertices.length / FLOATS_PER_VERTEX,
    );
    const vertexBuffer = this.#device.createBuffer({
      size: Math.max(vertices.byteLength, 4),
      usage: GPUBufferUsage.VERTEX | GPUBufferUsage.COPY_DST,
    });
    if (vertices.byteLength)
      this.#device.queue.writeBuffer(vertexBuffer, 0, vertices);
    const bindGroup = this.#device.createBindGroup({
      layout: this.#bindGroupLayout,
      entries: [
        { binding: 0, resource: this.#sampler },
        { binding: 1, resource: texture.createView() },
      ],
    });
    const encoder = this.#device.createCommandEncoder();
    const pass = encoder.beginRenderPass({
      colorAttachments: [
        {
          view: this.#context.getCurrentTexture().createView(),
          clearValue: { r: 0.025, g: 0.032, b: 0.045, a: 1 },
          loadOp: "clear",
          storeOp: "store",
        },
      ],
    });
    pass.setPipeline(this.#pipeline);
    pass.setBindGroup(0, bindGroup);
    pass.setVertexBuffer(0, vertexBuffer);
    pass.draw(vertices.length / FLOATS_PER_VERTEX);
    pass.end();
    this.#device.queue.submit([encoder.finish()]);
    void this.#device.queue.onSubmittedWorkDone().then(() => {
      vertexBuffer.destroy();
      texture.destroy();
    });
    void this.#device.popErrorScope().then((gpuError) => {
      if (gpuError) this.#canvas.dataset.gpuError = gpuError.message;
      else delete this.#canvas.dataset.gpuError;
    });
  }

  private resize(): void {
    const ratio = window.devicePixelRatio || 1;
    const width = Math.max(1, Math.floor(this.#canvas.clientWidth * ratio));
    const height = Math.max(1, Math.floor(this.#canvas.clientHeight * ratio));
    if (this.#canvas.width !== width || this.#canvas.height !== height) {
      this.#canvas.width = width;
      this.#canvas.height = height;
    }
  }
}

function buildAtlas(panes: PaneRenderModel[]): {
  canvas: OffscreenCanvas;
  glyphs: Map<string, [number, number]>;
} {
  const characters = new Set(" □");
  for (const pane of panes) {
    const snapshot = pane.snapshot;
    if (snapshot?.styledCells?.length) {
      for (const cell of snapshot.styledCells) {
        if (characters.size < MAX_ATLAS_GLYPHS && cell[2] && !isEmoji(cell[2]))
          characters.add(cell[2]);
      }
    } else {
      for (const character of snapshot?.contents ?? "") {
        if (
          characters.size < MAX_ATLAS_GLYPHS &&
          character !== "\n" &&
          !isEmoji(character)
        )
          characters.add(character);
      }
    }
  }
  const glyphs = new Map<string, [number, number]>();
  const rows = Math.max(1, Math.ceil(characters.size / ATLAS_COLUMNS));
  const canvas = new OffscreenCanvas(
    ATLAS_COLUMNS * ATLAS_CELL_WIDTH,
    rows * ATLAS_CELL_HEIGHT,
  );
  const context = canvas.getContext("2d", { alpha: true });
  if (!context) throw new Error("glyph atlasを作成できません");
  context.font = FONT;
  context.textBaseline = "top";
  context.fillStyle = "white";
  let index = 0;
  for (const character of characters) {
    const x = index % ATLAS_COLUMNS;
    const y = Math.floor(index / ATLAS_COLUMNS);
    glyphs.set(character, [x, y]);
    context.fillText(
      character,
      x * ATLAS_CELL_WIDTH,
      y * ATLAS_CELL_HEIGHT + 4,
    );
    index += 1;
  }
  return { canvas, glyphs };
}

function buildVertices(
  panes: PaneRenderModel[],
  glyphs: Map<string, [number, number]>,
  atlasRows: number,
  canvasWidth: number,
  canvasHeight: number,
): Float32Array {
  const values: number[] = [];
  for (const pane of panes) {
    const snapshot = pane.snapshot;
    if (!snapshot) continue;
    const viewport = terminalViewport(pane, snapshot.cols, snapshot.rows);
    const scale = viewport.scale;
    const offsetX = viewport.x;
    const offsetY = viewport.y;
    if (snapshot.styledCells?.length) {
      for (const cell of snapshot.styledCells) {
        const [
          row,
          column,
          rawText,
          cellWidth,
          foreground,
          background,
          attributes,
        ] = cell;
        if (cellWidth === 0) continue;
        const inverse = (attributes & 16) !== 0;
        let foregroundColor = rgbColor(foreground, [0.78, 0.84, 0.9, 1]);
        let backgroundColor = rgbColor(background, [0.025, 0.032, 0.045, 1]);
        if (inverse)
          [foregroundColor, backgroundColor] = [
            backgroundColor,
            foregroundColor,
          ];
        if (background !== null || inverse) {
          appendSolid(
            values,
            offsetX + column * CELL_WIDTH * scale,
            offsetY + row * CELL_HEIGHT * scale,
            CELL_WIDTH * Math.max(1, cellWidth) * scale,
            CELL_HEIGHT * scale,
            canvasWidth,
            canvasHeight,
            backgroundColor,
          );
        }
        if (rawText) {
          const character = isEmoji(rawText) ? "□" : rawText;
          const glyph = glyphs.get(character) ?? glyphs.get("□");
          if (glyph) {
            if ((attributes & 2) !== 0) foregroundColor[3] *= 0.55;
            appendGlyph(
              values,
              offsetX + column * CELL_WIDTH * scale,
              offsetY + row * CELL_HEIGHT * scale,
              CELL_WIDTH * Math.max(1, cellWidth) * scale,
              CELL_HEIGHT * scale,
              glyph,
              atlasRows,
              canvasWidth,
              canvasHeight,
              foregroundColor,
            );
          }
        }
        if ((attributes & 8) !== 0) {
          appendSolid(
            values,
            offsetX + column * CELL_WIDTH * scale,
            offsetY + (row + 0.86) * CELL_HEIGHT * scale,
            CELL_WIDTH * Math.max(1, cellWidth) * scale,
            Math.max(1, scale),
            canvasWidth,
            canvasHeight,
            foregroundColor,
          );
        }
      }
      if (!snapshot.hideCursor) {
        appendSolid(
          values,
          offsetX + snapshot.cursorCol * CELL_WIDTH * scale,
          offsetY + snapshot.cursorRow * CELL_HEIGHT * scale,
          CELL_WIDTH * scale,
          CELL_HEIGHT * scale,
          canvasWidth,
          canvasHeight,
          [0.38, 0.86, 0.75, 0.35],
        );
      }
      continue;
    }
    const lines = snapshot.contents.split("\n");
    lines.slice(0, snapshot.rows).forEach((line, row) => {
      let column = 0;
      for (const rawCharacter of line) {
        const character = isEmoji(rawCharacter) ? "□" : rawCharacter;
        const glyph = glyphs.get(character) ?? glyphs.get("□");
        if (!glyph) continue;
        const wide = isWide(character);
        const x = offsetX + column * CELL_WIDTH * scale;
        const y = offsetY + row * CELL_HEIGHT * scale;
        appendGlyph(
          values,
          x,
          y,
          CELL_WIDTH * (wide ? 2 : 1) * scale,
          CELL_HEIGHT * scale,
          glyph,
          atlasRows,
          canvasWidth,
          canvasHeight,
          [0.78, 0.84, 0.9, 1],
        );
        column += wide ? 2 : 1;
        if (column >= snapshot.cols) break;
      }
    });
  }
  return new Float32Array(values);
}

function appendGlyph(
  output: number[],
  x: number,
  y: number,
  width: number,
  height: number,
  glyph: [number, number],
  atlasRows: number,
  canvasWidth: number,
  canvasHeight: number,
  color: [number, number, number, number],
): void {
  const left = (x / canvasWidth) * 2 - 1;
  const right = ((x + width) / canvasWidth) * 2 - 1;
  const top = 1 - (y / canvasHeight) * 2;
  const bottom = 1 - ((y + height) / canvasHeight) * 2;
  const atlasWidth = ATLAS_COLUMNS * ATLAS_CELL_WIDTH;
  const u0 = (glyph[0] * ATLAS_CELL_WIDTH) / atlasWidth;
  const u1 = ((glyph[0] + 1) * ATLAS_CELL_WIDTH) / atlasWidth;
  const v0 = glyph[1] / atlasRows;
  const v1 = (glyph[1] + 1) / atlasRows;
  const vertex = (px: number, py: number, u: number, v: number) =>
    output.push(px, py, u, v, ...color, 0);
  vertex(left, top, u0, v0);
  vertex(left, bottom, u0, v1);
  vertex(right, bottom, u1, v1);
  vertex(left, top, u0, v0);
  vertex(right, bottom, u1, v1);
  vertex(right, top, u1, v0);
}

function appendSolid(
  output: number[],
  x: number,
  y: number,
  width: number,
  height: number,
  canvasWidth: number,
  canvasHeight: number,
  color: [number, number, number, number],
): void {
  const left = (x / canvasWidth) * 2 - 1;
  const right = ((x + width) / canvasWidth) * 2 - 1;
  const top = 1 - (y / canvasHeight) * 2;
  const bottom = 1 - ((y + height) / canvasHeight) * 2;
  const vertex = (px: number, py: number) =>
    output.push(px, py, 0, 0, ...color, 1);
  vertex(left, top);
  vertex(left, bottom);
  vertex(right, bottom);
  vertex(left, top);
  vertex(right, bottom);
  vertex(right, top);
}

function rgbColor(
  value: number | null,
  fallback: [number, number, number, number],
): [number, number, number, number] {
  if (value === null) return [...fallback];
  return [
    ((value >> 16) & 255) / 255,
    ((value >> 8) & 255) / 255,
    (value & 255) / 255,
    1,
  ];
}

function isWide(character: string): boolean {
  return /[\u1100-\u115f\u2e80-\ua4cf\uac00-\ud7a3\uf900-\ufaff\ufe10-\ufe6f\uff01-\uff60\uffe0-\uffe6]/u.test(
    character,
  );
}

function isEmoji(character: string): boolean {
  return /\p{Extended_Pictographic}|\p{Emoji_Presentation}/u.test(character);
}

const SHADER = /* wgsl */ `
struct VertexOutput {
  @builtin(position) position: vec4f,
  @location(0) uv: vec2f,
  @location(1) color: vec4f,
  @location(2) kind: f32,
};
@vertex fn vs_main(
  @location(0) position: vec2f,
  @location(1) uv: vec2f,
  @location(2) color: vec4f,
  @location(3) kind: f32,
) -> VertexOutput {
  var output: VertexOutput;
  output.position = vec4f(position, 0.0, 1.0);
  output.uv = uv;
  output.color = color;
  output.kind = kind;
  return output;
}
@group(0) @binding(0) var atlasSampler: sampler;
@group(0) @binding(1) var atlasTexture: texture_2d<f32>;
@fragment fn fs_main(input: VertexOutput) -> @location(0) vec4f {
  let alpha = textureSample(atlasTexture, atlasSampler, input.uv).a;
  if (input.kind > 0.5) { return input.color; }
  return vec4f(input.color.rgb, input.color.a * alpha);
}
`;
