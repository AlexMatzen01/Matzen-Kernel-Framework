/*
 * noVNC: HTML5 VNC client
 * Copyright (C) 2020 The noVNC authors
 * Licensed under MPL 2.0 (see LICENSE.txt)
 *
 * See README.md for usage and integration instructions.
 */

/*
 * HTML element utility functions
 */

export function clientToElement(x, y, elem) {
    const bounds = elem.getBoundingClientRect();
    let pos = { x: 0, y: 0 };
    // Pointer events use CSS pixels, while canvas.width/height are the
    // framebuffer dimensions. noVNC can scale the canvas with CSS when
    // fitting the remote display to the browser viewport, so convert the
    // local CSS coordinate back to intrinsic canvas pixels before sending
    // it to the server. Without this, the guest pointer drifts relative to
    // the browser cursor as the scale ratio differs from 1.
    const scaleX = elem.width && bounds.width ? elem.width / bounds.width : 1;
    const scaleY = elem.height && bounds.height ? elem.height / bounds.height : 1;
    // Clip to target bounds
    if (x < bounds.left) {
        pos.x = 0;
    } else if (x >= bounds.right) {
        pos.x = elem.width ? elem.width - 1 : bounds.width - 1;
    } else {
        pos.x = Math.floor((x - bounds.left) * scaleX);
    }
    if (y < bounds.top) {
        pos.y = 0;
    } else if (y >= bounds.bottom) {
        pos.y = elem.height ? elem.height - 1 : bounds.height - 1;
    } else {
        pos.y = Math.floor((y - bounds.top) * scaleY);
    }
    return pos;
}
