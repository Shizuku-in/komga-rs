<template>
  <v-navigation-drawer
    v-model="display"
    :location="$vuetify.rtl ? 'left' : 'right'"
    temporary
    disable-route-watcher
    class="fill-height"
  >
    <v-fab-transition>
      <v-btn
        v-if="clearButton" position="absolute" location="top right"
        icon
        :style="'z-index: 10; background: ' + $vuetify.theme.currentTheme.base"
        elevation="2"
        @click="clear"
        color="primary"
      >
        <v-icon>mdi-filter-variant-remove</v-icon>
      </v-btn>
    </v-fab-transition>

    <slot></slot>

    <template v-if="$slots.filter">
      <v-divider/>
      <v-list-subheader class="text-uppercase">{{ $t('filter_drawer.filter') }}</v-list-subheader>
    </template>
    <slot name="filter"></slot>

    <template v-if="$slots.sort">
      <v-divider/>
      <v-list-subheader class="text-uppercase">{{ $t('filter_drawer.sort') }}</v-list-subheader>
    </template>
    <slot name="sort"></slot>

  </v-navigation-drawer>
</template>

<script lang="ts">
import {defineComponent} from 'vue'

export default defineComponent({
  name: 'FilterDrawer',
  data: () => {
    return {
      display: false,
    }
  },
  props: {
    modelValue: Boolean,
    clearButton: {
      type: Boolean,
      default: false,
    },
  },
  watch: {
    modelValue(val) {
      this.display = val
    },
    display(val) {
      !val && this.$emit('update:modelValue', false)
    },
  },
  methods: {
    clear() {
      this.$emit('clear')
    },
  },
})
</script>

<style scoped>

</style>
