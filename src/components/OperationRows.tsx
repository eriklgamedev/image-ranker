import { useEffect, useId, useMemo, useRef, useState } from "react";
import { useVirtualizer } from "@tanstack/react-virtual";
import type { Operation, PlanRow } from "../native";

export const verificationLabel = (value?: string) =>
  value === "sha256-readback"
    ? "Output read-back SHA-256 verified"
    : value === "streamed-sha256"
      ? "SHA-256 checked while copying"
      : value
        ? value.replaceAll("-", " ")
        : "Not recorded";
export const rowStatus = (row: PlanRow, status: string) =>
  row.issue ||
  (row.noop
    ? "Unchanged / skipped"
    : row.step
      ? row.step.replaceAll("-", " ")
      : status === "complete"
        ? "Applied"
        : status === "planned"
          ? "Ready"
          : "Journaled");
const blocking = (row: PlanRow) => !!row.issue && !/^skip/i.test(row.issue);

/** Filters change visibility only: the parent keeps the native plan and approval. */
export function OperationRows({ operation }: { operation: Operation }) {
  const [search, setSearch] = useState("");
  const [filter, setFilter] = useState("all");
  const ref = useRef<HTMLDivElement>(null);
  const labelId = useId();
  const rows = useMemo(() => {
    const query = search.trim().toLocaleLowerCase();
    return operation.rows
      .map((row, index) => ({ row, index }))
      .filter(
        ({ row }) =>
          (filter === "all" ||
            (filter === "issues"
              ? blocking(row)
              : filter === "unchanged"
                ? row.noop || /^skip/i.test(row.issue || "")
                : !blocking(row) &&
                  !row.noop &&
                  !/^skip/i.test(row.issue || ""))) &&
          (!query ||
            [
              row.from,
              row.to,
              row.imageId,
              String(row.rank),
              String(row.outputNumber),
              rowStatus(row, operation.status),
            ].some((value) => value.toLocaleLowerCase().includes(query))),
      );
  }, [operation.rows, operation.status, search, filter]);
  const virtual = useVirtualizer({
    count: rows.length,
    getScrollElement: () => ref.current,
    estimateSize: () => 90,
    getItemKey: (index) => rows[index].index,
    overscan: 4,
    initialRect: { width: 900, height: 360 },
  });
  useEffect(() => {
    if (ref.current) ref.current.scrollTop = 0;
    virtual.measure();
  }, [search, filter, operation.id, virtual]);
  const visible = virtual.getVirtualItems();
  const columns =
    operation.kind === "copy" && operation.status !== "planned" ? 6 : 5;
  return (
    <section className="operation-rows" aria-label="Operation file rows">
      <div className="operation-row-controls">
        <label>
          Search file rows
          <input
            type="search"
            value={search}
            onChange={(event) => setSearch(event.target.value)}
            placeholder="Filename, path, rank or status"
          />
        </label>
        <label>
          Show rows
          <select
            value={filter}
            onChange={(event) => setFilter(event.target.value)}
          >
            <option value="all">All rows</option>
            <option value="issues">Blocking issues</option>
            <option value="changes">Changes</option>
            <option value="unchanged">Unchanged / skipped</option>
          </select>
        </label>
      </div>
      <p id={labelId} className="muted" role="status">
        Showing {rows.length.toLocaleString()} of{" "}
        {operation.rows.length.toLocaleString()} rows. Approval and issue checks
        always include all rows.
      </p>
      <div
        className="table-scroll operation-table-scroll"
        ref={ref}
        tabIndex={0}
        aria-label="Scrollable operation file rows"
        aria-describedby={labelId}
      >
        <table className="operation-table" aria-rowcount={rows.length + 1}>
          <thead>
            <tr>
              <th>Rank</th>
              <th>Output number</th>
              <th>Current file</th>
              <th>Destination</th>
              <th>Status</th>
              {columns === 6 && <th>Verification</th>}
            </tr>
          </thead>
          <tbody>
            {visible[0]?.start > 0 && (
              <tr aria-hidden="true">
                <td
                  colSpan={columns}
                  style={{ height: visible[0].start, padding: 0 }}
                />
              </tr>
            )}
            {visible.map((entry) => {
              const { row, index } = rows[entry.index];
              return (
                <tr
                  key={entry.key}
                  data-index={entry.index}
                  data-operation-row={index}
                  ref={virtual.measureElement}
                  className={blocking(row) ? "issue-row" : ""}
                  aria-rowindex={entry.index + 2}
                >
                  <td>{row.rank}</td>
                  <td>{row.outputNumber}</td>
                  <td title={row.from}>{row.from}</td>
                  <td title={row.to}>{row.to}</td>
                  <td>{rowStatus(row, operation.status)}</td>
                  {columns === 6 && (
                    <td>{verificationLabel(row.verification)}</td>
                  )}
                </tr>
              );
            })}
            {visible.length > 0 && (
              <tr aria-hidden="true">
                <td
                  colSpan={columns}
                  style={{
                    height: Math.max(
                      0,
                      virtual.getTotalSize() - visible[visible.length - 1].end,
                    ),
                    padding: 0,
                  }}
                />
              </tr>
            )}
          </tbody>
        </table>
        {!rows.length && (
          <p className="empty-preview">No file rows match these filters.</p>
        )}
      </div>
    </section>
  );
}
