// SPDX-License-Identifier: AGPL-3.0-or-later
// Small DOM helpers; a string from a server only ever reaches the page through textContent.

/**
 * @param {string} id
 * @returns {HTMLElement}
 */
export function el(id) {
  const node = document.getElementById(id);
  if (!node) {
    throw new Error(`missing element #${id}`);
  }
  return node;
}

/**
 * @param {string} id
 * @returns {HTMLInputElement}
 */
export function input(id) {
  const node = el(id);
  if (!(node instanceof HTMLInputElement)) {
    throw new Error(`#${id} is not an input`);
  }
  return node;
}

/**
 * @param {string} id
 * @returns {HTMLButtonElement}
 */
export function button(id) {
  const node = el(id);
  if (!(node instanceof HTMLButtonElement)) {
    throw new Error(`#${id} is not a button`);
  }
  return node;
}

/**
 * @param {string} id
 * @returns {HTMLDialogElement}
 */
export function dialog(id) {
  const node = el(id);
  if (!(node instanceof HTMLDialogElement)) {
    throw new Error(`#${id} is not a dialog`);
  }
  return node;
}

/**
 * @param {string} id
 * @returns {HTMLProgressElement}
 */
export function progressBar(id) {
  const node = el(id);
  if (!(node instanceof HTMLProgressElement)) {
    throw new Error(`#${id} is not a progress bar`);
  }
  return node;
}

/**
 * @param {HTMLElement} node
 * @param {boolean} visible
 */
export function show(node, visible) {
  node.hidden = !visible;
}

/**
 * @param {HTMLElement} node
 * @param {string} text
 */
export function setText(node, text) {
  node.textContent = text;
}

/**
 * Shows the node with the text, or hides it when there is none.
 * @param {HTMLElement} node
 * @param {string | null} text
 */
export function setNotice(node, text) {
  node.textContent = text ?? '';
  node.hidden = text === null;
}

/**
 * The text of a failed command, or of anything else that was thrown.
 * @param {unknown} error
 * @returns {string}
 */
export function messageOf(error) {
  if (typeof error === 'object' && error !== null && 'message' in error && typeof error.message === 'string') {
    return error.message;
  }
  return String(error);
}
