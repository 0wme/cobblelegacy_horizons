//! Pick Block on a server from before 1.21.4 (CobbleLegacy's fork: its servers are 1.21.1).
//!
//! Since 1.21.4 the client names the block or the entity (`pick_item_from_block`,
//! `pick_item_from_entity`) and the server chooses the slot. An older server has neither packet:
//! there `Minecraft.pickBlock` works on the client's own inventory. It selects the block's item
//! when the hotbar has it, asks the server to bring it from the main inventory (`pick_item`, which
//! is `Inventory.pickSlot`: the stack trades places with a hotbar slot's), or in creative puts a
//! new one in the hotbar (`handleCreativeModeItemAdd`). The protocol adapter drops a packet the
//! server does not have, so the middle click did nothing on those servers.
//!
//! The same is done here with what this client sends anyway: the selected slot
//! (`set_carried_item`), a hotbar swap click on the inventory menu where the old client sent
//! `pick_item` (the same exchange of two slots, which the server answers with its own), and the
//! creative slots every change of a creative inventory goes out as.
use super::Game;
use horizon_client::{items::ItemStack, pick_item};

/// The main inventory slot and the hotbar slot whose stacks traded places between `before` and
/// `after` (the 36 main slots, hotbar first); `None` when anything else changed.
pub(super) fn swapped(
    before: &[Option<ItemStack>],
    after: &[Option<ItemStack>],
) -> Option<(usize, usize)> {
    let changed: Vec<usize> = (0..before.len().min(after.len()))
        .filter(|&i| before[i] != after[i])
        .collect();
    match changed[..] {
        [hotbar, slot]
            if hotbar < 9
                && slot >= 9
                && after[hotbar] == before[slot]
                && after[slot] == before[hotbar] =>
        {
            Some((slot, hotbar))
        }
        _ => None,
    }
}

impl Game<'_> {
    /// `Minecraft.pickBlock` of 1.21.3 and older for `stack`, the block's clone item or the
    /// entity's pick result: the inventory changes here as [`pick_item::pick`] has it, and the
    /// server hears of it. In creative the changed slots go out as creative slots with the tick;
    /// in survival the only change is `pickSlot`'s swap, which the server makes too.
    pub(super) fn remote_pick_locally(&mut self, stack: &ItemStack) {
        let before = self.inventory[..36].to_vec();
        let infinite = self.creative;
        if !pick_item::pick(
            &mut self.inventory[..36],
            &mut self.selected,
            stack,
            infinite,
        ) {
            return;
        }
        if !infinite && let Some((slot, hotbar)) = swapped(&before, &self.inventory[..36]) {
            self.remote_swap_into_hotbar(slot, hotbar);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stack(item: &str, count: u32) -> Option<ItemStack> {
        Some(ItemStack {
            item: format!("minecraft:{item}").into(),
            count,
            damage: 0,
            extra: None,
        })
    }
    fn inventory(filled: &[(usize, &str)]) -> Vec<Option<ItemStack>> {
        let mut items = vec![None; 36];
        for &(i, item) in filled {
            items[i] = stack(item, 5);
        }
        items
    }
    /// The inventory after picking stone, the selected slot, and what the server is asked.
    fn picked(
        mut items: Vec<Option<ItemStack>>,
        mut selected: usize,
        infinite: bool,
    ) -> (Vec<Option<ItemStack>>, usize, Option<(usize, usize)>) {
        let before = items.clone();
        let stone = stack("stone", 1).unwrap();
        pick_item::pick(&mut items, &mut selected, &stone, infinite);
        let swap = swapped(&before, &items);
        (items, selected, swap)
    }

    #[test]
    fn a_stack_of_the_main_inventory_is_brought_by_a_swap_with_the_hotbar() {
        // Survival: the first free hotbar slot from the selected one takes it.
        let (items, selected, swap) = picked(inventory(&[(0, "dirt"), (20, "stone")]), 0, false);
        assert_eq!((selected, swap), (1, Some((20, 1))));
        assert_eq!(
            (items[1].clone(), items[20].clone()),
            (stack("stone", 5), None)
        );
        // A full hotbar: the selected slot's stack goes where the stone was.
        let mut full: Vec<_> = (0..9).map(|_| stack("dirt", 5)).collect();
        full.extend((9..36).map(|i| if i == 30 { stack("stone", 5) } else { None }));
        let (items, selected, swap) = picked(full, 7, false);
        assert_eq!((selected, swap), (7, Some((30, 7))));
        assert_eq!(items[30], stack("dirt", 5));
    }

    #[test]
    fn selecting_a_hotbar_stack_or_finding_none_asks_the_server_nothing() {
        let hotbar = inventory(&[(0, "dirt"), (6, "stone"), (20, "stone")]);
        let (items, selected, swap) = picked(hotbar.clone(), 2, false);
        assert_eq!((items, selected, swap), (hotbar, 6, None));
        let without = inventory(&[(0, "dirt")]);
        let (items, selected, swap) = picked(without.clone(), 0, false);
        assert_eq!((items, selected, swap), (without, 0, None));
    }

    #[test]
    fn a_creative_pick_is_not_a_swap() {
        // A new stack in the hotbar, and with a full hotbar the slot's own moved to a free one:
        // creative slots say both, no click.
        let (items, selected, swap) = picked(inventory(&[(0, "dirt")]), 0, true);
        assert_eq!((selected, swap), (1, None));
        assert_eq!(items[1], stack("stone", 1));
        let mut full: Vec<_> = (0..9).map(|_| stack("dirt", 5)).collect();
        full.extend(vec![None; 27]);
        let (items, selected, swap) = picked(full, 4, true);
        assert_eq!((selected, swap), (4, None));
        assert_eq!(
            (items[4].clone(), items[9].clone()),
            (stack("stone", 1), stack("dirt", 5))
        );
    }
}
