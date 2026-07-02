// src/codes.ts
var eofbCode = [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1];
var passCode = [0, 0, 0, 1];
var verticalCode0 = [1];
var verticalCodeR1 = [0, 1, 1];
var verticalCodeR2 = [0, 0, 0, 0, 1, 1];
var verticalCodeR3 = [0, 0, 0, 0, 0, 1, 1];
var verticalCodeL1 = [0, 1, 0];
var verticalCodeL2 = [0, 0, 0, 0, 1, 0];
var verticalCodeL3 = [0, 0, 0, 0, 0, 1, 0];
var whiteTerminatingCodes_64 = [
  [0, 0, 1, 1, 0, 1, 0, 1],
  // 0
  [0, 0, 0, 1, 1, 1],
  // 1
  [0, 1, 1, 1],
  // 2
  [1, 0, 0, 0],
  // and so on...
  [1, 0, 1, 1],
  [1, 1, 0, 0],
  [1, 1, 1, 0],
  [1, 1, 1, 1],
  [1, 0, 0, 1, 1],
  [1, 0, 1, 0, 0],
  [0, 0, 1, 1, 1],
  [0, 1, 0, 0, 0],
  [0, 0, 1, 0, 0, 0],
  [0, 0, 0, 0, 1, 1],
  [1, 1, 0, 1, 0, 0],
  [1, 1, 0, 1, 0, 1],
  [1, 0, 1, 0, 1, 0],
  [1, 0, 1, 0, 1, 1],
  [0, 1, 0, 0, 1, 1, 1],
  [0, 0, 0, 1, 1, 0, 0],
  [0, 0, 0, 1, 0, 0, 0],
  [0, 0, 1, 0, 1, 1, 1],
  [0, 0, 0, 0, 0, 1, 1],
  [0, 0, 0, 0, 1, 0, 0],
  [0, 1, 0, 1, 0, 0, 0],
  [0, 1, 0, 1, 0, 1, 1],
  [0, 0, 1, 0, 0, 1, 1],
  [0, 1, 0, 0, 1, 0, 0],
  [0, 0, 1, 1, 0, 0, 0],
  [0, 0, 0, 0, 0, 0, 1, 0],
  [0, 0, 0, 0, 0, 0, 1, 1],
  [0, 0, 0, 1, 1, 0, 1, 0],
  [0, 0, 0, 1, 1, 0, 1, 1],
  [0, 0, 0, 1, 0, 0, 1, 0],
  [0, 0, 0, 1, 0, 0, 1, 1],
  [0, 0, 0, 1, 0, 1, 0, 0],
  [0, 0, 0, 1, 0, 1, 0, 1],
  [0, 0, 0, 1, 0, 1, 1, 0],
  [0, 0, 0, 1, 0, 1, 1, 1],
  [0, 0, 1, 0, 1, 0, 0, 0],
  [0, 0, 1, 0, 1, 0, 0, 1],
  [0, 0, 1, 0, 1, 0, 1, 0],
  [0, 0, 1, 0, 1, 0, 1, 1],
  [0, 0, 1, 0, 1, 1, 0, 0],
  [0, 0, 1, 0, 1, 1, 0, 1],
  [0, 0, 0, 0, 0, 1, 0, 0],
  [0, 0, 0, 0, 0, 1, 0, 1],
  [0, 0, 0, 0, 1, 0, 1, 0],
  [0, 0, 0, 0, 1, 0, 1, 1],
  [0, 1, 0, 1, 0, 0, 1, 0],
  [0, 1, 0, 1, 0, 0, 1, 1],
  [0, 1, 0, 1, 0, 1, 0, 0],
  [0, 1, 0, 1, 0, 1, 0, 1],
  [0, 0, 1, 0, 0, 1, 0, 0],
  [0, 0, 1, 0, 0, 1, 0, 1],
  [0, 1, 0, 1, 1, 0, 0, 0],
  [0, 1, 0, 1, 1, 0, 0, 1],
  [0, 1, 0, 1, 1, 0, 1, 0],
  [0, 1, 0, 1, 1, 0, 1, 1],
  [0, 1, 0, 0, 1, 0, 1, 0],
  [0, 1, 0, 0, 1, 0, 1, 1],
  [0, 0, 1, 1, 0, 0, 1, 0],
  [0, 0, 1, 1, 0, 0, 1, 1],
  [0, 0, 1, 1, 0, 1, 0, 0]
  // 63
];
var blackTerminatingCodes_64 = [
  [0, 0, 0, 0, 1, 1, 0, 1, 1, 1],
  // 0
  [0, 1, 0],
  // 1
  [1, 1],
  // 2
  [1, 0],
  // and so on...
  [0, 1, 1],
  [0, 0, 1, 1],
  [0, 0, 1, 0],
  [0, 0, 0, 1, 1],
  [0, 0, 0, 1, 0, 1],
  [0, 0, 0, 1, 0, 0],
  [0, 0, 0, 0, 1, 0, 0],
  [0, 0, 0, 0, 1, 0, 1],
  [0, 0, 0, 0, 1, 1, 1],
  [0, 0, 0, 0, 0, 1, 0, 0],
  [0, 0, 0, 0, 0, 1, 1, 1],
  [0, 0, 0, 0, 1, 1, 0, 0, 0],
  [0, 0, 0, 0, 0, 1, 0, 1, 1, 1],
  [0, 0, 0, 0, 0, 1, 1, 0, 0, 0],
  [0, 0, 0, 0, 0, 0, 1, 0, 0, 0],
  [0, 0, 0, 0, 1, 1, 0, 0, 1, 1, 1],
  [0, 0, 0, 0, 1, 1, 0, 1, 0, 0, 0],
  [0, 0, 0, 0, 1, 1, 0, 1, 1, 0, 0],
  [0, 0, 0, 0, 0, 1, 1, 0, 1, 1, 1],
  [0, 0, 0, 0, 0, 1, 0, 1, 0, 0, 0],
  [0, 0, 0, 0, 0, 0, 1, 0, 1, 1, 1],
  [0, 0, 0, 0, 0, 0, 1, 1, 0, 0, 0],
  [0, 0, 0, 0, 1, 1, 0, 0, 1, 0, 1, 0],
  [0, 0, 0, 0, 1, 1, 0, 0, 1, 0, 1, 1],
  [0, 0, 0, 0, 1, 1, 0, 0, 1, 1, 0, 0],
  [0, 0, 0, 0, 1, 1, 0, 0, 1, 1, 0, 1],
  [0, 0, 0, 0, 0, 1, 1, 0, 1, 0, 0, 0],
  [0, 0, 0, 0, 0, 1, 1, 0, 1, 0, 0, 1],
  [0, 0, 0, 0, 0, 1, 1, 0, 1, 0, 1, 0],
  [0, 0, 0, 0, 0, 1, 1, 0, 1, 0, 1, 1],
  [0, 0, 0, 0, 1, 1, 0, 1, 0, 0, 1, 0],
  [0, 0, 0, 0, 1, 1, 0, 1, 0, 0, 1, 1],
  [0, 0, 0, 0, 1, 1, 0, 1, 0, 1, 0, 0],
  [0, 0, 0, 0, 1, 1, 0, 1, 0, 1, 0, 1],
  [0, 0, 0, 0, 1, 1, 0, 1, 0, 1, 1, 0],
  [0, 0, 0, 0, 1, 1, 0, 1, 0, 1, 1, 1],
  [0, 0, 0, 0, 0, 1, 1, 0, 1, 1, 0, 0],
  [0, 0, 0, 0, 0, 1, 1, 0, 1, 1, 0, 1],
  [0, 0, 0, 0, 1, 1, 0, 1, 1, 0, 1, 0],
  [0, 0, 0, 0, 1, 1, 0, 1, 1, 0, 1, 1],
  [0, 0, 0, 0, 0, 1, 0, 1, 0, 1, 0, 0],
  [0, 0, 0, 0, 0, 1, 0, 1, 0, 1, 0, 1],
  [0, 0, 0, 0, 0, 1, 0, 1, 0, 1, 1, 0],
  [0, 0, 0, 0, 0, 1, 0, 1, 0, 1, 1, 1],
  [0, 0, 0, 0, 0, 1, 1, 0, 0, 1, 0, 0],
  [0, 0, 0, 0, 0, 1, 1, 0, 0, 1, 0, 1],
  [0, 0, 0, 0, 0, 1, 0, 1, 0, 0, 1, 0],
  [0, 0, 0, 0, 0, 1, 0, 1, 0, 0, 1, 1],
  [0, 0, 0, 0, 0, 0, 1, 0, 0, 1, 0, 0],
  [0, 0, 0, 0, 0, 0, 1, 1, 0, 1, 1, 1],
  [0, 0, 0, 0, 0, 0, 1, 1, 1, 0, 0, 0],
  [0, 0, 0, 0, 0, 0, 1, 0, 0, 1, 1, 1],
  [0, 0, 0, 0, 0, 0, 1, 0, 1, 0, 0, 0],
  [0, 0, 0, 0, 0, 1, 0, 1, 1, 0, 0, 0],
  [0, 0, 0, 0, 0, 1, 0, 1, 1, 0, 0, 1],
  [0, 0, 0, 0, 0, 0, 1, 0, 1, 0, 1, 1],
  [0, 0, 0, 0, 0, 0, 1, 0, 1, 1, 0, 0],
  [0, 0, 0, 0, 0, 1, 0, 1, 1, 0, 1, 0],
  [0, 0, 0, 0, 0, 1, 1, 0, 0, 1, 1, 0],
  [0, 0, 0, 0, 0, 1, 1, 0, 0, 1, 1, 1]
  // 63
];
var whiteTerminatingCodes_64_1728 = [
  [64, [1, 1, 0, 1, 1]],
  [128, [1, 0, 0, 1, 0]],
  [192, [0, 1, 0, 1, 1, 1]],
  [256, [0, 1, 1, 0, 1, 1, 1]],
  [320, [0, 0, 1, 1, 0, 1, 1, 0]],
  [384, [0, 0, 1, 1, 0, 1, 1, 1]],
  [448, [0, 1, 1, 0, 0, 1, 0, 0]],
  [512, [0, 1, 1, 0, 0, 1, 0, 1]],
  [576, [0, 1, 1, 0, 1, 0, 0, 0]],
  [640, [0, 1, 1, 0, 0, 1, 1, 1]],
  [704, [0, 1, 1, 0, 0, 1, 1, 0, 0]],
  [768, [0, 1, 1, 0, 0, 1, 1, 0, 1]],
  [832, [0, 1, 1, 0, 1, 0, 0, 1, 0]],
  [896, [0, 1, 1, 0, 1, 0, 0, 1, 1]],
  [960, [0, 1, 1, 0, 1, 0, 1, 0, 0]],
  [1024, [0, 1, 1, 0, 1, 0, 1, 0, 1]],
  [1088, [0, 1, 1, 0, 1, 0, 1, 1, 0]],
  [1152, [0, 1, 1, 0, 1, 0, 1, 1, 1]],
  [1216, [0, 1, 1, 0, 1, 1, 0, 0, 0]],
  [1280, [0, 1, 1, 0, 1, 1, 0, 0, 1]],
  [1344, [0, 1, 1, 0, 1, 1, 0, 1, 0]],
  [1408, [0, 1, 1, 0, 1, 1, 0, 1, 1]],
  [1472, [0, 1, 0, 0, 1, 1, 0, 0, 0]],
  [1536, [0, 1, 0, 0, 1, 1, 0, 0, 1]],
  [1600, [0, 1, 0, 0, 1, 1, 0, 1, 0]],
  [1664, [0, 1, 1, 0, 0, 0]],
  [1728, [0, 1, 0, 0, 1, 1, 0, 1, 1]]
];
var blackTerminatingCodes_64_1728 = [
  [64, [0, 0, 0, 0, 0, 0, 1, 1, 1, 1]],
  [128, [0, 0, 0, 0, 1, 1, 0, 0, 1, 0, 0, 0]],
  [192, [0, 0, 0, 0, 1, 1, 0, 0, 1, 0, 0, 1]],
  [256, [0, 0, 0, 0, 0, 1, 0, 1, 1, 0, 1, 1]],
  [320, [0, 0, 0, 0, 0, 0, 1, 1, 0, 0, 1, 1]],
  [384, [0, 0, 0, 0, 0, 0, 1, 1, 0, 1, 0, 0]],
  [448, [0, 0, 0, 0, 0, 0, 1, 1, 0, 1, 0, 1]],
  [512, [0, 0, 0, 0, 0, 0, 1, 1, 0, 1, 1, 0, 0]],
  [576, [0, 0, 0, 0, 0, 0, 1, 1, 0, 1, 1, 0, 1]],
  [640, [0, 0, 0, 0, 0, 0, 1, 0, 0, 1, 0, 1, 0]],
  [704, [0, 0, 0, 0, 0, 0, 1, 0, 0, 1, 0, 1, 1]],
  [768, [0, 0, 0, 0, 0, 0, 1, 0, 0, 1, 1, 0, 0]],
  [832, [0, 0, 0, 0, 0, 0, 1, 0, 0, 1, 1, 0, 1]],
  [896, [0, 0, 0, 0, 0, 0, 1, 1, 1, 0, 0, 1, 0]],
  [960, [0, 0, 0, 0, 0, 0, 1, 1, 1, 0, 0, 1, 1]],
  [1024, [0, 0, 0, 0, 0, 0, 1, 1, 1, 0, 1, 0, 0]],
  [1088, [0, 0, 0, 0, 0, 0, 1, 1, 1, 0, 1, 0, 1]],
  [1152, [0, 0, 0, 0, 0, 0, 1, 1, 1, 0, 1, 1, 0]],
  [1216, [0, 0, 0, 0, 0, 0, 1, 1, 1, 0, 1, 1, 1]],
  [1280, [0, 0, 0, 0, 0, 0, 1, 0, 1, 0, 0, 1, 0]],
  [1344, [0, 0, 0, 0, 0, 0, 1, 0, 1, 0, 0, 1, 1]],
  [1408, [0, 0, 0, 0, 0, 0, 1, 0, 1, 0, 1, 0, 0]],
  [1472, [0, 0, 0, 0, 0, 0, 1, 0, 1, 0, 1, 0, 1]],
  [1536, [0, 0, 0, 0, 0, 0, 1, 0, 1, 1, 0, 1, 0]],
  [1600, [0, 0, 0, 0, 0, 0, 1, 0, 1, 1, 0, 1, 1]],
  [1664, [0, 0, 0, 0, 0, 0, 1, 1, 0, 0, 1, 0, 0]],
  [1728, [0, 0, 0, 0, 0, 0, 1, 1, 0, 0, 1, 0, 1]]
];
var makeupCodes = [
  [1792, [0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0]],
  [1856, [0, 0, 0, 0, 0, 0, 0, 1, 1, 0, 0]],
  [1920, [0, 0, 0, 0, 0, 0, 0, 1, 1, 0, 1]],
  [1984, [0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 1, 0]],
  [2048, [0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 1, 1]],
  [2112, [0, 0, 0, 0, 0, 0, 0, 1, 0, 1, 0, 0]],
  [2176, [0, 0, 0, 0, 0, 0, 0, 1, 0, 1, 0, 1]],
  [2240, [0, 0, 0, 0, 0, 0, 0, 1, 0, 1, 1, 0]],
  [2304, [0, 0, 0, 0, 0, 0, 0, 1, 0, 1, 1, 1]],
  [2368, [0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 0, 0]],
  [2432, [0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 0, 1]],
  [2496, [0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 0]],
  [2560, [0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 1]]
];
var lastMakeupCode = makeupCodes[makeupCodes.length - 1];
function toHorizonalCode(a0value, a0a1, a1a2) {
  const code = [0, 0, 1];
  for (let i = 1; i <= a0a1 / lastMakeupCode[0]; i++) {
    code.push(...lastMakeupCode[1]);
  }
  let a0a1rest = a0a1 % lastMakeupCode[0];
  for (let i = makeupCodes.length - 1; i >= 0; i--) {
    if (a0a1rest >= makeupCodes[i][0]) {
      code.push(...makeupCodes[i][1]);
      a0a1rest -= makeupCodes[i][0];
      break;
    }
  }
  if (a0value === 0) {
    for (let i = whiteTerminatingCodes_64_1728.length - 1; i >= 0; i--) {
      if (a0a1rest >= whiteTerminatingCodes_64_1728[i][0]) {
        code.push(...whiteTerminatingCodes_64_1728[i][1]);
        a0a1rest -= whiteTerminatingCodes_64_1728[i][0];
        break;
      }
    }
    code.push(...whiteTerminatingCodes_64[a0a1rest]);
  } else {
    for (let i = blackTerminatingCodes_64_1728.length - 1; i >= 0; i--) {
      if (a0a1rest >= blackTerminatingCodes_64_1728[i][0]) {
        code.push(...blackTerminatingCodes_64_1728[i][1]);
        a0a1rest -= blackTerminatingCodes_64_1728[i][0];
        break;
      }
    }
    code.push(...blackTerminatingCodes_64[a0a1rest]);
  }
  for (let i = 1; i <= a1a2 / lastMakeupCode[0]; i++) {
    code.push(...lastMakeupCode[1]);
  }
  let a1a2rest = a1a2 % lastMakeupCode[0];
  for (let i = makeupCodes.length - 1; i >= 0; i--) {
    if (a1a2rest >= makeupCodes[i][0]) {
      code.push(...makeupCodes[i][1]);
      a1a2rest -= makeupCodes[i][0];
      break;
    }
  }
  if (a0value === 0) {
    for (let i = blackTerminatingCodes_64_1728.length - 1; i >= 0; i--) {
      if (a1a2rest >= blackTerminatingCodes_64_1728[i][0]) {
        code.push(...blackTerminatingCodes_64_1728[i][1]);
        a1a2rest -= blackTerminatingCodes_64_1728[i][0];
        break;
      }
    }
    code.push(...blackTerminatingCodes_64[a1a2rest]);
  } else {
    for (let i = whiteTerminatingCodes_64_1728.length - 1; i >= 0; i--) {
      if (a1a2rest >= whiteTerminatingCodes_64_1728[i][0]) {
        code.push(...whiteTerminatingCodes_64_1728[i][1]);
        a1a2rest -= whiteTerminatingCodes_64_1728[i][0];
        break;
      }
    }
    code.push(...whiteTerminatingCodes_64[a1a2rest]);
  }
  return code;
}

// src/encode.ts
function findNextValueChange(buffer, start, width, startValue) {
  let prevValue = start === 0 ? 0 : null;
  for (let i = start; i < width; i++) {
    const byteIndex = Math.floor(i / 8);
    const byte = buffer[byteIndex];
    if (byte === 0 && prevValue === 0 || byte === 255 && prevValue === 1) {
      i += 7 - i % 8;
      continue;
    } else {
      const bit = (byte & 1 << i % 8) !== 0 ? 1 : 0;
      if (prevValue !== null && bit !== prevValue && bit !== startValue) {
        return i;
      }
      prevValue = bit;
    }
  }
  return width;
}
function encode(imageBuffer, width, height) {
  const referenceBuffer = new Uint8Array(Math.ceil(width / 8));
  let outBuffer = new Uint8Array(Math.ceil(width * height / 8));
  let outBufferLength = 0;
  let addingBits = [];
  function addOctetBits() {
    const octets = Math.floor(addingBits.length / 8);
    if (outBufferLength + octets > outBuffer.length) {
      const oldBuffer = outBuffer;
      outBuffer = new Uint8Array(Math.ceil((outBufferLength + octets) * 1.5));
      outBuffer.set(oldBuffer.slice(0, outBufferLength));
    }
    const bytes = [];
    for (let i = 0; i < octets; i++) {
      let byte = 0;
      for (let j = 0; j < 8; j++) {
        byte |= (addingBits[i * 8 + j] ? 1 : 0) << 7 - j;
      }
      outBuffer[outBufferLength++] = byte;
      bytes.push(byte);
    }
    const remainingBits = addingBits.slice(octets * 8);
    addingBits = remainingBits;
  }
  for (let y = 0; y < height; y++) {
    const lineBuffer = new Uint8Array(Math.ceil(width / 8));
    for (let x = 0; x < width; x++) {
      const totalBitIndex = y * width + x;
      const byteIndex = Math.floor(totalBitIndex / 8);
      const byte = imageBuffer[byteIndex];
      const bit = (byte & 1 << 7 - totalBitIndex % 8) !== 0;
      if (bit) {
        lineBuffer[Math.floor(x / 8)] |= 1 << x % 8;
      }
    }
    let a0 = 0;
    let a0value = 0;
    while (a0 < width) {
      const a1 = findNextValueChange(lineBuffer, a0, width, a0value);
      const b1 = findNextValueChange(referenceBuffer, a0, width, a0value);
      const b2 = findNextValueChange(referenceBuffer, b1, width, a0value === 0 ? 1 : 0);
      const a0a1 = a1 - a0;
      const a1b1 = b1 - a1;
      if (b2 < a1) {
        addingBits.push(...passCode);
        a0 = b2;
      } else if (-3 <= a1b1 && a1b1 <= 3) {
        let code = [];
        if (a1b1 === 0) code = verticalCode0;
        else if (a1b1 === 1) code = verticalCodeL1;
        else if (a1b1 === 2) code = verticalCodeL2;
        else if (a1b1 === 3) code = verticalCodeL3;
        else if (a1b1 === -1) code = verticalCodeR1;
        else if (a1b1 === -2) code = verticalCodeR2;
        else if (a1b1 === -3) code = verticalCodeR3;
        addingBits.push(...code);
        a0 = a1;
        a0value = a0value === 0 ? 1 : 0;
      } else {
        const a2 = findNextValueChange(lineBuffer, a1, width, a0value === 0 ? 1 : 0);
        const a1a2 = a2 - a1;
        const code = toHorizonalCode(a0value, a0a1, a1a2);
        addingBits.push(...code);
        a0 = a2;
      }
      addOctetBits();
    }
    referenceBuffer.set(lineBuffer, 0);
  }
  addingBits.push(...eofbCode);
  addOctetBits();
  if (addingBits.length > 0) {
    let byte = 0;
    for (let j = 0; j < addingBits.length; j++) {
      byte |= (addingBits[j] ? 1 : 0) << 7 - j;
    }
    outBuffer[outBufferLength++] = byte;
    addingBits = [];
  }
  const finalBuffer = outBuffer.slice(0, outBufferLength);
  return finalBuffer;
}

// src/utils.ts
function binarizeToBitPacked(imageData, threshold = 128) {
  const { data, width, height } = imageData;
  const pixelCount = width * height;
  const byteLength = Math.ceil(pixelCount / 8);
  const bitPacked = new Uint8Array(byteLength);
  for (let i = 0; i < pixelCount; i++) {
    const baseIndex = i * 4;
    const r = data[baseIndex];
    const g = data[baseIndex + 1];
    const b = data[baseIndex + 2];
    const a = data[baseIndex + 3];
    const alpha = a / 255;
    const rBlended = r * alpha + 255 * (1 - alpha);
    const gBlended = g * alpha + 255 * (1 - alpha);
    const bBlended = b * alpha + 255 * (1 - alpha);
    const luminance = 0.299 * rBlended + 0.587 * gBlended + 0.114 * bBlended;
    const bit = luminance < threshold ? 1 : 0;
    const byteIndex = Math.floor(i / 8);
    const bitOffset = 7 - i % 8;
    bitPacked[byteIndex] |= bit << bitOffset;
  }
  return bitPacked;
}
function imageDataToEncoded(imageData, threshold = 128) {
  const bitPacked = binarizeToBitPacked(imageData, threshold);
  return encode(bitPacked, imageData.width, imageData.height);
}
export {
  binarizeToBitPacked,
  encode,
  imageDataToEncoded
};
