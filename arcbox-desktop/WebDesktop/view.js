// Zoom policy for the remote desktop, shared by the browser and regression tests.
//
// "follow": the machine's resolution follows the window. Zoom sets how large its
// desktop looks: at 50% the machine gets twice the window's size in pixels (sharp on
// Retina), at 200% half of it. The picture always fits the window.
// "fixed": the machine keeps its resolution. "fit" scales the picture into the
// window; a percentage shows it at that size and scrolls when it is larger.
export const zoomSteps = [0.5, 0.67, 0.75, 0.8, 0.9, 1, 1.1, 1.25, 1.5, 1.75, 2];
export const defaultView = Object.freeze({ resize: 'follow', zoom: 1 });

export function parseView(text) {
    try {
        const value = JSON.parse(text);
        const resize = value?.resize === 'fixed' ? 'fixed' : 'follow';
        const zoom = value?.zoom === 'fit' && resize === 'fixed' ? 'fit'
            : zoomSteps.includes(value?.zoom) ? value.zoom : defaultView.zoom;
        return { resize, zoom };
    } catch {
        return { ...defaultView };
    }
}

// The next step from the current zoom; `current` may be the live scale of a fitted picture.
export function stepZoom(current, direction) {
    if (direction > 0) return zoomSteps.find(step => step > current + 0.001) ?? zoomSteps.at(-1);
    return zoomSteps.findLast(step => step < current - 0.001) ?? zoomSteps[0];
}

// The resolution to ask the machine for, given the window's desktop area in CSS pixels.
export function remoteSize(width, height, zoom) {
    return {
        w: Math.max(320, Math.min(8192, Math.floor(width / zoom))),
        h: Math.max(240, Math.min(8192, Math.floor(height / zoom))),
    };
}

// A followed resolution the machine refused is fitted instead, so show the real scale.
export function zoomLabel(view, scale) {
    const fitted = view.resize === 'follow' && Math.abs(scale - view.zoom) > 0.02;
    if (view.zoom === 'fit' || fitted) return '适应 ' + Math.round(scale * 100) + '%';
    return Math.round(view.zoom * 100) + '%';
}
