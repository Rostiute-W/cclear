// 空间分析页的图形视图：用 treemap 画当前节点的直接子目录，点格子进入下一级。
import { useEffect, useRef, useState } from "react";
import { hierarchy, treemap, treemapSquarify } from "d3-hierarchy";
import type { HierarchyRectangularNode } from "d3-hierarchy";
import { formatSize } from "../format";
import type { DirNode } from "../types";

// 只布局最大的这些子目录，其余合并成“其他”，否则小格子小到看不清也点不中。
const MAX_CELLS = 60;
const MIN_HEIGHT = 320;
const MAX_HEIGHT = 560;

// 相邻格子的底色，按顺序取，保证相邻区域颜色不同。
const FILLS = [
  "#cfe0fb",
  "#d8ece0",
  "#fbe3c4",
  "#e5dcf7",
  "#fbd9d5",
  "#d3ecf4",
  "#eaeecb",
  "#f3dcec",
];

interface TreemapProps {
  nodes: DirNode[];
  onEnter: (node: DirNode) => void;
}

interface Cell {
  name: string;
  value: number;
  // 进入下一级用的节点；合并出来的“其他”没有节点，不能点进去。
  node: DirNode | null;
}

interface CellDatum extends Cell {
  children?: CellDatum[];
}

interface Rect {
  cell: Cell;
  x0: number;
  y0: number;
  x1: number;
  y1: number;
}

export function Treemap({ nodes, onEnter }: TreemapProps) {
  const container = useRef<HTMLDivElement>(null);
  const [width, setWidth] = useState(0);

  // 窗口尺寸变化时要重新布局，所以监听容器宽度，而不是进页面时量一次。
  useEffect(() => {
    const element = container.current;
    if (!element) {
      return;
    }

    const observer = new ResizeObserver(() => setWidth(element.clientWidth));
    observer.observe(element);
    setWidth(element.clientWidth);

    return () => observer.disconnect();
  }, []);

  const height = Math.max(MIN_HEIGHT, Math.min(MAX_HEIGHT, width * 0.55));
  const rects = layout(nodes, width, height);

  return (
    <div className="treemap" ref={container}>
      {rects.length === 0 ? (
        <p className="muted">这个目录下没有可显示的子目录。</p>
      ) : (
        <svg width={width} height={height} className="treemap-svg">
          {rects.map((rect, index) => {
            const cellWidth = rect.x1 - rect.x0;
            const cellHeight = rect.y1 - rect.y0;
            const node = rect.cell.node;
            const enterable = node !== null && node.hasChildren;

            return (
              <g
                key={`${rect.cell.name}-${index}`}
                className={enterable ? "treemap-cell" : "treemap-cell treemap-cell-static"}
                onClick={enterable ? () => onEnter(node) : undefined}
              >
                <rect
                  x={rect.x0}
                  y={rect.y0}
                  width={cellWidth}
                  height={cellHeight}
                  rx={3}
                  fill={FILLS[index % FILLS.length]}
                />
                <title>{`${rect.cell.name}\n${formatSize(rect.cell.value)}`}</title>
                {cellWidth > 64 && cellHeight > 34 && (
                  <>
                    <text x={rect.x0 + 6} y={rect.y0 + 17} className="treemap-label">
                      {shorten(rect.cell.name, cellWidth)}
                    </text>
                    <text x={rect.x0 + 6} y={rect.y0 + 31} className="treemap-size">
                      {formatSize(rect.cell.value)}
                    </text>
                  </>
                )}
              </g>
            );
          })}
        </svg>
      )}
    </div>
  );
}

// 把子目录压成格子数据：取前 60 个，剩下的合并成“其他”。
function cellsOf(nodes: DirNode[]): Cell[] {
  const cells: Cell[] = nodes
    .slice(0, MAX_CELLS)
    .map((node) => ({ name: node.name, value: node.allocated, node }));
  const rest = nodes.slice(MAX_CELLS);

  if (rest.length > 0) {
    cells.push({
      name: `其他（${rest.length} 项）`,
      value: rest.reduce((sum, node) => sum + node.allocated, 0),
      node: null,
    });
  }

  return cells;
}

function layout(nodes: DirNode[], width: number, height: number): Rect[] {
  if (width <= 0 || height <= 0 || nodes.length === 0) {
    return [];
  }

  const children = cellsOf(nodes);
  const total = children.reduce((sum, cell) => sum + cell.value, 0);
  // 全是 0 字节时 d3 会算出 NaN 坐标，直接不画。
  if (total <= 0) {
    return [];
  }

  const root = hierarchy<CellDatum>({ name: "", value: 0, node: null, children }).sum(
    (datum) => datum.value,
  );
  const laid = treemap<CellDatum>()
    .tile(treemapSquarify)
    .size([width, height])
    .paddingInner(2)
    .paddingOuter(2)(root);
  // 所有格子都在第一层，treemap 会为每个子节点填上矩形坐标。
  const cells = (laid.children ?? []) as HierarchyRectangularNode<CellDatum>[];

  return cells.map((child) => ({
    cell: child.data,
    x0: child.x0,
    y0: child.y0,
    x1: child.x1,
    y1: child.y1,
  }));
}

// SVG 里没法自动省略，按可用宽度粗略截断字符。
function shorten(name: string, cellWidth: number): string {
  const maxChars = Math.floor((cellWidth - 12) / 8);
  if (name.length <= maxChars) {
    return name;
  }

  return `${name.slice(0, Math.max(1, maxChars - 1))}…`;
}
