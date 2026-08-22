// 拦截 global fetch:统计发往 openrouter responses 的真实请求体规模
// 工具结果的载荷在 function_call_output.output 字段
const origFetch = globalThis.fetch;
let n = 0;
const points = [];
globalThis.fetch = async (url, init = {}) => {
  if (typeof url === "string" && url.includes("openrouter.ai/api/v1/responses") && init?.body) {
    n += 1;
    try {
      const body = JSON.parse(init.body);
      const items = Array.isArray(body.input) ? body.input : [];
      let chars = JSON.stringify(body.system ?? "").length;
      for (const item of items) {
        chars += JSON.stringify(item).length;  // 原样序列化 = 真实载荷
      }
      points.push({ n, items: items.length, chars });
      console.log(`[probe] req#${n}: items=${items.length} payload_chars=${chars}`);
    } catch {}
  }
  return origFetch(url, init);
};
process.env.EXPORT_POINTS = "1";
await import("/root/Anchor/scripts/experiments/run-econ-exp.mjs");
const { writeFile } = await import("node:fs/promises");
