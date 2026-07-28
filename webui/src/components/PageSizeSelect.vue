<template>
  <v-menu location="bottom">
    <template v-slot:activator="{ props }">
      <v-btn icon v-bind="props">
        <v-icon>mdi-view-grid-plus</v-icon>
      </v-btn>
    </template>
    <v-list>
      <v-list-item
        v-for="item in items"
        :key="item"
        :active="item === modelValue"
        @click="setPageSize(item)"
      >
        <v-list-item-title>{{ item }}</v-list-item-title>
      </v-list-item>
    </v-list>
  </v-menu>
</template>

<script lang="ts">
import {defineComponent} from 'vue'

export default defineComponent({
  name: 'PageSizeSelect',
  props: {
    items: {
      type: Array as () => number[],
      default: () => [20, 50, 100, 200, 500],
    },
    modelValue: {
      type: Number,
      required: true,
    },
  },
  methods: {
    setPageSize (size: number) {
      this.$emit('update:modelValue', size)
    },
  },
})
</script>

<style scoped>

</style>
