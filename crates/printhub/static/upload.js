// The upload form: a drop target, slicing settings hidden for G-code that needs none, and an
// STL measured in the browser so its size and a scale can be seen before anything is sent.
(() => {
  // Reading the whole file is the only way to its bounding box; past this a phone would
  // rather not, and the size line is a convenience.
  const MEASURE_LIMIT = 64 * 1024 * 1024;

  const form = document.querySelector("form.upload");
  if (!form) return;
  const input = form.querySelector('input[type="file"]');
  const zone = form.querySelector(".dropzone");
  const slicing = form.querySelector(".slicing");
  const size = form.querySelector(".model-size");
  const scale = form.querySelector('input[name="scale_percent"]');

  // Without this a file dropped next to the form replaces the page with the model.
  for (const event of ["dragover", "drop"]) {
    document.addEventListener(event, (e) => e.preventDefault());
  }

  // dragenter and dragleave both fire again for every child element under the pointer, so the
  // highlight follows a counter rather than the last event.
  let depth = 0;
  const highlight = (on) => zone.classList.toggle("dragging", on);
  zone.addEventListener("dragenter", (e) => {
    e.preventDefault();
    depth += 1;
    highlight(true);
  });
  zone.addEventListener("dragover", (e) => e.preventDefault());
  zone.addEventListener("dragleave", () => {
    depth = Math.max(0, depth - 1);
    if (depth === 0) highlight(false);
  });
  zone.addEventListener("drop", (e) => {
    e.preventDefault();
    e.stopPropagation();
    depth = 0;
    highlight(false);
    if (e.dataTransfer.files.length > 0) {
      input.files = e.dataTransfer.files;
      input.dispatchEvent(new Event("change"));
    }
  });

  let measured = null;
  input.addEventListener("change", () => {
    const file = input.files[0];
    const gcode = file && file.name.toLowerCase().endsWith(".gcode");
    if (slicing) slicing.hidden = Boolean(gcode);
    measured = null;
    show();
    if (file && !gcode) {
      measure(file).then((box) => {
        if (input.files[0] === file) {
          measured = box;
          show();
        }
      });
    }
  });
  if (scale) scale.addEventListener("input", show);

  function show() {
    if (!size) return;
    size.hidden = measured === null;
    if (measured === null) return;
    const factor = Number(scale ? scale.value : 100) / 100;
    const at = measured.map((mm) => mm.toFixed(1)).join(" × ");
    if (!Number.isFinite(factor) || factor <= 0 || factor === 1) {
      size.textContent = `The model measures ${at} mm.`;
    } else {
      const scaled = measured.map((mm) => (mm * factor).toFixed(1)).join(" × ");
      size.textContent = `The model measures ${at} mm, printed at ${scaled} mm.`;
    }
  }

  async function measure(file) {
    if (file.size > MEASURE_LIMIT) return null;
    try {
      const buffer = await file.arrayBuffer();
      // A binary STL is exactly 84 bytes plus 50 per triangle. The leading "solid" does not tell
      // the two formats apart: exporters write it into the binary header too.
      const view = new DataView(buffer);
      const triangles = buffer.byteLength >= 84 ? view.getUint32(80, true) : 0;
      const box =
        buffer.byteLength === 84 + 50 * triangles
          ? binaryBox(view, triangles)
          : asciiBox(new TextDecoder().decode(buffer));
      return box;
    } catch {
      return null;
    }
  }

  function binaryBox(view, triangles) {
    const min = [Infinity, Infinity, Infinity];
    const max = [-Infinity, -Infinity, -Infinity];
    for (let t = 0; t < triangles; t += 1) {
      // 84-byte header, then per triangle a normal (3 floats) that carries no position.
      let at = 84 + t * 50 + 12;
      for (let corner = 0; corner < 3; corner += 1) {
        for (let axis = 0; axis < 3; axis += 1) {
          const value = view.getFloat32(at, true);
          at += 4;
          if (value < min[axis]) min[axis] = value;
          if (value > max[axis]) max[axis] = value;
        }
      }
    }
    return extent(min, max);
  }

  function asciiBox(text) {
    const min = [Infinity, Infinity, Infinity];
    const max = [-Infinity, -Infinity, -Infinity];
    const vertex = /^\s*vertex\s+(\S+)\s+(\S+)\s+(\S+)/gm;
    for (const match of text.matchAll(vertex)) {
      for (let axis = 0; axis < 3; axis += 1) {
        const value = Number(match[axis + 1]);
        if (!Number.isFinite(value)) return null;
        if (value < min[axis]) min[axis] = value;
        if (value > max[axis]) max[axis] = value;
      }
    }
    return extent(min, max);
  }

  function extent(min, max) {
    const sizes = [0, 1, 2].map((axis) => max[axis] - min[axis]);
    return sizes.every((mm) => Number.isFinite(mm) && mm >= 0) ? sizes : null;
  }
})();
