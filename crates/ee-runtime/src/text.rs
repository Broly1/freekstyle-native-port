//! Text changes for the PC version: saves live on disk, so messages about
//! memory cards, slots and not removing the card are rewritten.
//!
//! `ENGLISH.DAT` is a flat table of NUL-terminated strings. Every rewrite
//! is no longer than the original and is padded with spaces split around
//! it, so string offsets, indices and centring stay as they were.

/// (original, replacement). `\xae` is the game font's (R) character.
const REWRITES: &[(&[u8], &[u8])] = &[
    (b"This will overwrite the current game state with that stored on the memory card.  Continue?", b"This will overwrite the current game state with the saved game.  Continue?"),
    (b"Checking memory card.", b"Checking save data."),
    (b"Memory Card (PS2) Status", b"Save Data Status"),
    (b"Would you like to format this unformatted Memory Card (PS2)?", b"Would you like to reset the save data?"),
    (b"Formatting.  Please do not remove the Memory Card (PS2) or disconnect power.", b"Resetting save data."),
    (b"Unformatted Memory Card (PS2)", b"No Save Data"),
    (b"Format completed successfully.", b"Save data reset."),
    (b"No Memory Card (PS2).", b"No save data."),
    (b"Loading.  Please do not remove the Memory Card (PS2) or disconnect power, as doing so may cause game data loss.", b"Loading."),
    (b"No saved game was located on this Memory Card (PS2).", b"No saved game was found."),
    (b"Saving.  Please do not remove the Memory Card (PS2) or disconnect power, as doing so may cause game data loss.", b"Saving."),
    (b"Checking memory card (8MB) (for PlayStation\xae2) in MEMORY CARD slot 1.", b"Checking save data."),
    (b"No memory card (8MB) (for PlayStation\xae2) in MEMORY CARD slot 1.", b"No save data."),
    (b"The memory card (8MB) (for PlayStation\xae2) in MEMORY CARD slot 1 is not formatted.", b"The save data could not be read."),
    (b"No game file found on memory card (8MB) (for PlayStation\xae2) in MEMORY CARD slot 1.", b"No saved game found."),
    (b"Reading memory card (8MB) (for PlayStation\xae2) in MEMORY CARD slot 1.", b"Loading saved game."),
    (b"DO NOT REMOVE, Turn the Power Off, or Reset Console.", b"Please wait."),
    (b"Problem encountered while reading game file from memory card (8MB) (for PlayStation\xae2) in MEMORY CARD slot 1.", b"Problem encountered while loading the saved game."),
    (b"Not enough room. A memory card (8MB) (for PlayStation\xae2) with 9999KB available is required.", b"Not enough room. 9999KB of free disk space is required."),
    (b"Do you wish to save to memory card (8MB) (for PlayStation\xae2) in MEMORY CARD slot 1, overwriting current -Freekstyle- save game file?", b"Do you wish to save, overwriting the current -Freekstyle- save game?"),
    (b"Writing to memory card (8MB) (for PlayStation\xae2) in MEMORY CARD slot 1. DO NOT REMOVE, Turn the Power Off, or Reset Console.", b"Saving game."),
    (b"Problem encountered while writing game file to memory card (8MB) (for PlayStation\xae2) in MEMORY CARD slot 1.", b"Problem encountered while saving the game."),
    (b"The memory card (8MB) (for PlayStation\xae2) in MEMORY CARD slot 1 is already formatted.", b"The save data is already reset."),
    (b"Formatting memory card (8MB) (for PlayStation\xae2) in MEMORY CARD slot 1. DO NOT REMOVE, Turn the Power Off, or Reset Console.", b"Resetting save data."),
    (b"Format failed.", b"Reset failed."),
    (b"Memory card (8MB) (for PlayStation\xae2) in MEMORY CARD slot 1 is not formatted. Do you wish to format?", b"The save data could not be read. Do you wish to reset it?"),
    (b"Reading memory card (8MB) (for PlayStation\xae2) in MEMORY CARD slot 1. DO NOT REMOVE, Turn the Power Off, or Reset Console.", b"Loading saved game."),
    (b"Do you want to overwrite the current -Freekstyle- save game on the memory card (8MB) (for PlayStation\xae2) in MEMORY CARD slot 1?", b"Do you want to overwrite the current -Freekstyle- save game?"),
    (b"A memory card (8MB) (for PlayStation\xae2) with 9999KB available is required to save your progress.", b"9999KB of free disk space is required to save your progress."),
    (b"This will replace all current User ID's and game settings with those on the memory card (8MB) (for PlayStation\xae2) in MEMORY CARD slot 1. Do you wish to continue?", b"This will replace all current User ID's and game settings with the saved ones. Do you wish to continue?"),
    (b"Formatting", b"Resetting"),
    (b"Press X to begin", b"Press A to begin"),
    (b"There is no save data on the memory card (8MB) (for PlayStation\xae2) in MEMORY CARD slot 1. Would you like to save?", b"There is no saved game. Would you like to save?"),
    (b"Do you wish to save to memory card (8MB) (for PlayStation\xae2) in MEMORY CARD slot 1?", b"Do you wish to save?"),
    (b"This will merge all User ID's on memory card (8MB) (for PlayStation\xae2) in MEMORY CARD slot 1 with the current User ID's.", b"This will merge all saved User ID's with the current User ID's."),
    (b"Insufficient space on memory card (8MB) (for PlayStation\xae2) in memory card slot 1.  At least 9999KB required.", b"Insufficient disk space.  At least 9999KB required."),
];

/// Rewrite memory card messages in a string table, in place.
pub fn patch_strings(data: &mut [u8]) -> usize {
    let mut changed = 0;
    let mut start = 0;
    while start < data.len() {
        let end = data[start..].iter().position(|&b| b == 0).map_or(data.len(), |p| start + p);
        let s = &data[start..end];
        if let Some((_, new)) = REWRITES.iter().find(|(old, _)| *old == s) {
            let len = s.len();
            debug_assert!(new.len() <= len);
            let pad = len - new.len();
            let left = pad / 2;
            let mut out = vec![b' '; len];
            out[left..left + new.len()].copy_from_slice(new);
            data[start..end].copy_from_slice(&out);
            changed += 1;
        }
        start = end + 1;
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrites_fit() {
        for (old, new) in REWRITES {
            assert!(new.len() <= old.len(), "{}", String::from_utf8_lossy(new));
        }
    }

    #[test]
    fn keeps_layout() {
        let mut table = b"EMPTY TEXT\0Checking memory card.\0Format failed.\0Options\0".to_vec();
        let before = table.clone();
        assert_eq!(patch_strings(&mut table), 2);
        assert_eq!(table.len(), before.len());
        let positions = |t: &[u8]| t.iter().enumerate().filter(|(_, &b)| b == 0).map(|(i, _)| i).collect::<Vec<_>>();
        assert_eq!(positions(&table), positions(&before));
        let strings: Vec<_> = table.split(|&b| b == 0).map(|s| String::from_utf8_lossy(s).into_owned()).collect();
        assert_eq!(strings[1].trim(), "Checking save data.");
        assert_eq!(strings[2].trim(), "Reset failed.");
        assert_eq!(strings[3], "Options");
    }
}
